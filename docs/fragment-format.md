# Fragment Format Specification

Authoritative specification for the bootc-assemble fragment format.

## Fragment Image Anatomy

A fragment is a single-layer OCI image with this directory structure under `/fragment/`:

```
/fragment/
├── fragment.toml      # Required: metadata and package declarations
├── tree/              # Optional: files to overlay into target image
│   ├── etc/yum.repos.d/*.repo
│   ├── etc/pki/rpm-gpg/RPM-GPG-KEY-*
│   └── ...            # Arbitrary filesystem paths
└── scripts/           # Optional: post-install configuration
    └── configure.sh   # Only this script is executed
```

## `fragment.toml` Schema

```toml
[fragment]
name = "tailscale"                    # Required: unique identifier (ASCII, no spaces)
version = "1.82.0"                    # Required: semantic version or date
description = "Tailscale VPN client"  # Required: one-line summary
vendor = "Tailscale Inc."             # Optional: provider name
phase = "config"                      # Required: "repos" or "config"

[fragment.conflicts]
# Optional: fragment names this fragment conflicts with
fragments = ["wireguard", "zerotier"]

[fragment.packages]
# Optional: packages this fragment can install (informational only)
available = ["tailscale"]
```

### Field Constraints

- `name`: Must be unique within a manifest. Used for conflict detection and stage naming.
- `version`: Any string. Not validated, purely informational.
- `description`: Single-line text. Displayed by `inspect` and `list`.
- `vendor`: Optional. Identifies the fragment publisher.
- `phase`: Must be `"repos"` or `"config"`. Controls execution order.
  - `repos` (weight 10): Runs before packages are installed. Tree content is restricted to repo definitions and GPG keys (paths under `etc/yum.repos.d/` or `etc/pki/rpm-gpg/`). Must not contain `scripts/configure.sh`.
  - `config` (weight 30): Runs after packages are installed. No tree restrictions. May contain scripts.
- `conflicts.fragments`: Optional array of fragment names this fragment is incompatible with. Assembly fails if any listed fragment is present in the manifest.
- `packages.available`: Optional array of package names this fragment can install. Not enforced — used by `inspect` and for future dependency analysis.

## `tree/` Directory Layout

The `tree/` directory mirrors the target image's filesystem. During assembly, its contents are copied verbatim into the target image via `COPY --from=<fragment> /fragment/tree/ /`.

Example mapping:
```
tree/etc/yum.repos.d/tailscale.repo  →  /etc/yum.repos.d/tailscale.repo
tree/etc/pki/rpm-gpg/RPM-GPG-KEY-TS  →  /etc/pki/rpm-gpg/RPM-GPG-KEY-TS
tree/usr/lib/systemd/system/ts.service → /usr/lib/systemd/system/ts.service
```

Phase-specific restrictions:
- `repos` fragments: Tree may only contain paths under `etc/yum.repos.d/` or `etc/pki/rpm-gpg/`. Other paths cause assembly to fail.
- `config` fragments: No restrictions.

The generated Containerfile applies fragments in this order:
1. Repo files (yum.repos.d, rpm-gpg) from all fragments
2. Packages (single batched `dnf install` with all requested packages)
3. Config files (full `tree/` content from config-phase fragments)
4. Scripts (configure.sh execution)
5. Preset application and validation

Config files land after package installation to ensure fragment-supplied configurations are never overwritten by RPM defaults.

## `scripts/configure.sh` Contract

A fragment may contain a shell script at `/fragment/scripts/configure.sh`. If present, the tool executes it after all packages are installed:

```dockerfile
COPY --from=<fragment> /fragment/scripts/configure.sh /tmp/frag-<name>-configure.sh
RUN /tmp/frag-<name>-configure.sh && rm -f /tmp/frag-<name>-configure.sh
```

### Rules

1. `configure.sh` is the only script entrypoint. Other scripts in `scripts/` are ignored unless sourced by `configure.sh`.
2. The script runs as root in the target image's filesystem.
3. The script must not call `dnf`, `yum`, or other package managers. Packages are installed by the manifest, not by scripts.
4. The script should be idempotent where possible (though it only runs once during build).
5. Exit code 0 = success. Nonzero exits fail the build.

Typical uses:
- Apply systemd presets (`systemctl preset <service>`)
- Create users/groups
- Template configuration files
- Set permissions or capabilities

## Containerfile.fragment Build Pattern

Fragments are built with this single-layer pattern:

```dockerfile
FROM scratch
COPY fragment.toml tree/ scripts/ /fragment/
```

No `RUN` commands, no base image. The fragment carries only the files needed for assembly.

Build and push:
```bash
podman build -f Containerfile.fragment -t quay.io/user/fragment:1.0 .
podman push quay.io/user/fragment:1.0
```

## OCI Annotations (Fast-Path Metadata)

For performance, fragments should include OCI annotations that mirror `fragment.toml` fields. When present, `inspect` and `list` can read metadata without pulling layers.

Annotation keys:
- `io.bootc.fragment.name` — fragment name
- `io.bootc.fragment.version` — version string
- `io.bootc.fragment.description` — description text
- `io.bootc.fragment.vendor` — vendor name (optional)
- `io.bootc.fragment.phase` — `"repos"` or `"config"`
- `io.bootc.fragment.provides.repos` — JSON array of repo IDs (e.g., `["epel"]`)
- `io.bootc.fragment.packages.available` — JSON array of package names

Annotations are **not** used during assembly — the tool always parses the in-layer `fragment.toml` for the authoritative fragment definition. Annotations are a read-only optimization.

Set annotations during build:
```bash
podman build --annotation io.bootc.fragment.name=tailscale \
             --annotation io.bootc.fragment.version=1.82.0 \
             --annotation io.bootc.fragment.phase=config \
             -f Containerfile.fragment -t quay.io/user/tailscale:1.82.0 .
```

## Manifest YAML Schema

Manifests declare the base image and fragment composition:

```yaml
apiVersion: bootc.io/v1alpha1
kind: Composition

base: quay.io/centos-bootc/centos-bootc:stream10

fragments:
  - image: quay.io/example/epel:10
    packages: [htop, tmux]
  - image: quay.io/example/tailscale:1.82.0
    packages: [tailscale]
    mirror: s/download.tailscale.com/mirror.internal.corp/g
```

### Fields

- `apiVersion`: Must be `bootc.io/v1alpha1`
- `kind`: Must be `Composition`
- `base`: Required. Base bootc or RHCOS image reference.
- `fragments`: Required. Array of at least one fragment.
  - `image`: Required. OCI image reference (`registry/repo:tag`) or digest (`registry/repo@sha256:...`).
  - `packages`: Optional. Array of package names to install from this fragment's repos. Defaults to `[]`.
  - `mirror`: Optional. `sed` expression to rewrite repo baseurl/metalink in this fragment's `.repo` files (e.g., `s/cdn.redhat.com/satellite.corp/g`).

Package installation is deduplicated across all fragments — if multiple fragments request the same package, it's installed once.

Repo deduplication: If multiple fragments provide `.repo` files with the same filename, the tool compares their content. Identical content is silently deduplicated (first fragment wins). Different content for the same repo ID causes the build to fail.
