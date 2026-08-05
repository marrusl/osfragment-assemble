# Fragment Format Specification

Authoritative specification for the osfragment-assemble fragment format.

## Fragment Image Anatomy

A fragment is an OCI image with this directory structure under `/fragment/`:

```
/fragment/
├── fragment.toml      # Required: metadata and package declarations
├── tree/              # Optional: files to overlay into target image
│   ├── etc/yum.repos.d/*.repo
│   ├── etc/pki/rpm-gpg/RPM-GPG-KEY-*
│   └── ...            # Arbitrary filesystem paths
└── hooks/             # Optional: build-time setup (any language)
    ├── entrypoint     # Required when hooks/ has content; the only file run
    └── lib/helper.sh  # Support material, never invoked by the tool
```

## `fragment.toml` Schema

`fragment.toml` is unit metadata. Its fields describe the fragment: identity, version, publisher, ordering, and how it combines with other fragments. System configuration lives in `tree/` (files, verbatim) and `hooks/` (executables, any language), in whatever formats already exist; the tool never parses that content. See [Design Rationales](rationales.md#why-the-fragment-format-is-as-light-as-possible-and-no-simpler).

```toml
[fragment]
name = "tailscale"                    # Required: unique identifier (ASCII, no spaces)
version = "1.82.0"                    # Required: semantic version or date
description = "Tailscale VPN client"  # Required: one-line summary
vendor = "Tailscale Inc."             # Optional: provider name

[fragment.conflicts]
# Optional: fragment names this fragment conflicts with
fragments = ["wireguard", "zerotier"]

[fragment.packages]
# Optional: packages this fragment forces during assembly
required = ["tailscale"]
```

### Field Constraints

- `name`: Must be unique within a manifest. Used for conflict detection and stage naming.
- `version`: Any string. Not validated, purely informational.
- `description`: Single-line text. Displayed by `inspect` and `list`.
- `vendor`: Optional. Identifies the fragment publisher.
- `conflicts.fragments`: Optional array of fragment names this fragment is incompatible with. Assembly fails if any listed fragment is present in the manifest.
- `packages.required`: Optional array of package names this fragment forces during assembly. These packages are always installed, even without a manifest entry. Unknown keys in `[fragment.packages]` are rejected as parse errors.

## `tree/` Directory Layout

The `tree/` directory mirrors the target image's filesystem. During assembly, its contents are copied verbatim into the target image via `COPY --from=<fragment> /fragment/tree/ /`.

Example mapping:
```
tree/etc/yum.repos.d/tailscale.repo  →  /etc/yum.repos.d/tailscale.repo
tree/etc/pki/rpm-gpg/RPM-GPG-KEY-TS  →  /etc/pki/rpm-gpg/RPM-GPG-KEY-TS
tree/usr/lib/systemd/system/ts.service → /usr/lib/systemd/system/ts.service
```

Where a file lands is decided by its path, not by any declaration. Repo
definitions and GPG keys (`etc/yum.repos.d/`, `etc/pki/rpm-gpg/`) are hoisted
ahead of the package install; everything else under `tree/` is copied after it.
A fragment carrying both gets both treatments.

The generated Containerfile applies fragments in this order:
1. Repo files (yum.repos.d, rpm-gpg) from all fragments
2. Packages (single batched `dnf install` with all requested packages)
3. Config files (the rest of each fragment's `tree/` content)
4. Hooks (each hook-carrying fragment's `hooks/entrypoint`)
5. Preset application and validation

Config files land after package installation to ensure fragment-supplied configurations are never overwritten by RPM defaults.

## Hooks Contract

If `/fragment/hooks/` contains any file, it must contain an executable regular file named `entrypoint`. That file is the only thing osfragment-assemble runs: once, as root, after packages are installed, with no arguments and no environment beyond what the build already provides. Fragment authors are responsible for setting the execute bit and for any required interpreters being available in the image at build time. The generated Containerfile bind-mounts the fragment's `hooks/` directory for the duration of a single `RUN` and invokes the entrypoint through it:

```dockerfile
RUN --mount=type=bind,from=<fragment>,source=/fragment/hooks,target=/frag-hooks,z \
    /frag-hooks/entrypoint
```

The hooks are never copied into the image. A bind mount is not committed to a layer, so no hook bytes remain in the built image and there is nothing to clean up afterwards. Copying the directory and deleting it in a later `RUN` would not achieve this: the delete only writes a whiteout, and the bytes stay recoverable in the `COPY` layer.

Under `--self-contained` the hooks are mounted from the build context instead of the fragment image, as `source=fragments/<name>/hooks`, with no `from=`. The mount is otherwise identical to the form above.

### Rules

1. `hooks/entrypoint` is the only file executed. Everything else under `hooks/`, at any depth, is support material available to it at `/frag-hooks/`: helper scripts, vendor installers, payload binaries. Sequencing, arguments, and conditionals belong inside the entrypoint, which is a real program and has control flow.
2. A fragment whose `hooks/` holds files but no executable `hooks/entrypoint` fails to load, with an error naming the fragment. There is no fallback to running whatever looks runnable. A nested `hooks/lib/entrypoint` does not satisfy the rule; only `hooks/entrypoint` counts.
3. Hooks run as root in the target image's filesystem.
4. Packages declared in the manifest are preferred; the tool can deduplicate and batch them. Hooks are not prevented from installing packages, but hook-installed packages bypass deduplication and won't appear in the manifest's package list.
5. Hooks should be idempotent where possible (though they only run once during build).
6. Exit code 0 = success. Nonzero exits fail the build.

Typical uses:
- Apply systemd presets (`systemctl preset <service>`)
- Create users/groups
- Template configuration files
- Set permissions or capabilities

## Containerfile.fragment Build Pattern

Fragments are built with this pattern:

```dockerfile
FROM scratch
COPY fragment.toml /fragment/
COPY tree/ /fragment/tree/
COPY hooks/ /fragment/hooks/
```

No `RUN` commands, no base image. The fragment carries only the files needed for assembly. Omit the `tree/` or `hooks/` line if the fragment has no such directory.

Each directory needs its own `COPY` with an explicit destination. A single `COPY fragment.toml tree/ hooks/ /fragment/` copies the *contents* of `tree/` and `hooks/` into `/fragment/`, so neither `/fragment/tree/` nor `/fragment/hooks/` exists and the fragment reads as empty. That form builds and pushes without error, so the mistake surfaces only when the tool loads the fragment.

Build and push:
```bash
podman build -f Containerfile.fragment -t quay.io/user/fragment:1.0 .
podman push quay.io/user/fragment:1.0
```

## OCI Annotations (Fast-Path Metadata)

For performance, fragments should include OCI annotations that mirror `fragment.toml` fields. When present, `inspect` and `list` can read metadata without pulling layers.

Annotation keys:
- `com.github.marrusl.osfragment.name`: fragment name
- `com.github.marrusl.osfragment.version`: version string
- `com.github.marrusl.osfragment.description`: description text
- `com.github.marrusl.osfragment.vendor`: vendor name (optional)
- `com.github.marrusl.osfragment.provides.repos`: JSON array of repo IDs (e.g., `["epel"]`)
- `com.github.marrusl.osfragment.packages.required`: JSON array of required package names

Annotations are **not** used during assembly; the tool always parses the in-layer `fragment.toml` for the authoritative fragment definition. Annotations are a read-only optimization.

Set annotations during build:
```bash
podman build --annotation com.github.marrusl.osfragment.name=tailscale \
             --annotation com.github.marrusl.osfragment.version=1.82.0 \
             --annotation 'com.github.marrusl.osfragment.packages.required=["tailscale"]' \
             -f Containerfile.fragment -t quay.io/user/tailscale:1.82.0 .
```

## Manifest YAML Schema

Manifests declare the base image and fragment composition:

```yaml
apiVersion: osfragment/v1alpha1
kind: Composition

base: quay.io/centos-bootc/centos-bootc:stream10
baseType: bootc  # Optional: override automatic base image classification

fragments:
  - image: quay.io/example/epel:10
    packages: [htop, tmux]
  - image: quay.io/example/tailscale:1.82.0
    packages: [tailscale]
    mirror: s/download.tailscale.com/mirror.internal.corp/g
```

### Fields

- `apiVersion`: Must be `osfragment/v1alpha1`
- `kind`: Must be `Composition`
- `base`: Required. Base bootc or RHCOS image reference.
- `baseType`: Optional. Overrides automatic base image classification. Values: `bootc` or `container`.
  When set, skips label inspection entirely. When absent, the tool inspects the base image's
  `containers.bootc` label to determine classification. See README for the full classification order.
  - `bootc`: Base image is bootc-compatible. The generated Containerfile includes `systemctl preset-all`
    and `bootc container lint` steps.
  - `container`: Base image is a plain container. These steps are omitted.
- `fragments`: Required. Array of at least one fragment.
  - `image`: Required. OCI image reference (`registry/repo:tag`) or digest (`registry/repo@sha256:...`).
  - `packages`: Optional. Array of package names to install from this fragment's repos. Defaults to `[]`.
  - `mirror`: Optional. `sed` expression to rewrite repo baseurl/metalink in this fragment's `.repo` files (e.g., `s/cdn.redhat.com/satellite.corp/g`).

Package installation is deduplicated across all fragments; if multiple fragments request the same package, it's installed once.

Repo deduplication: If multiple fragments provide `.repo` files with the same filename, the tool compares their content. Identical content is silently deduplicated (first fragment wins). Different content for the same repo ID causes the build to fail.
