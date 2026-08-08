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
├── mount/             # Optional: material bind-mounted during the package step
│   └── etc/pki/entitlement/*.pem
└── hooks/             # Optional: build-time setup (any language)
    ├── entrypoint     # Required when hooks/ has content; the only file run
    └── lib/helper.sh  # Support material, never invoked by the tool
```

## `fragment.toml` Schema

`fragment.toml` is unit metadata. Its fields describe the fragment: identity, version, publisher, ordering, and how it combines with other fragments. System configuration lives in `tree/` (files, verbatim) and `hooks/` (executables, any language), in whatever formats already exist; the tool never parses that content. See [Design Rationales](rationales.md#why-the-fragment-format-is-as-light-as-possible-and-no-simpler).

```toml
[fragment]
name = "tailscale"                    # Required: unique identifier (lowercase, see Field Constraints)
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

- `name`: Must match `[a-z0-9]([a-z0-9._-]*[a-z0-9])?`, 1 to 64 characters: lowercase ASCII letters and digits, optionally separated by `.`, `-`, or `_`, starting and ending with a letter or digit. Must be unique within a manifest. Used for conflict detection, the fragment's directory name under `fragments/` in `--self-contained` output, and stage naming (the tool emits `--from=frag-<name>` into the generated Containerfile).

  A name that doesn't match the grammar is rejected, not rewritten: silently rewriting it into something safe would produce a build that doesn't match what the fragment author wrote. Uppercase is rejected deliberately, not just as a byproduct of the character set: Containerfile stage names are case-insensitive to the builder, so `Foo` and `foo` in one manifest would otherwise collide into a single stage. Rejecting uppercase closes that collision and matches the lowercase convention already used throughout OCI naming.
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

## `mount/` Directory Layout

The `mount/` subtree mirrors target paths exactly as `tree/` does. Detection is presence-based, with no `fragment.toml` section. Derivation collects every directory that directly contains a file and drops any nested inside another, so `mount/etc/rhsm/rhsm.conf` plus `mount/etc/rhsm/ca/cert.pem` yields one mount of `/etc/rhsm`. A regular file directly under `mount/` is a generation error. An empty `mount/` produces a notice.

The emitted form is:

```dockerfile
RUN --mount=type=bind,from=<fragment>@sha256:...,source=/fragment/mount/etc/pki/entitlement,target=/etc/pki/entitlement,ro,z \
    dnf install -y \
        some-package \
    && dnf clean all
```

with the self-contained variant reading `source=fragments/<name>/mount/<path>` and no `from=`.

The manifest entry for a fragment carrying `mount/` must be pinned by digest. Two fragments mounting colliding targets is an error, as is a target that equals or contains `/etc/yum.repos.d` or `/etc/pki/rpm-gpg`. Symlinks and hardlinks are rejected in fragment layers, `mount/` included.

The builder never commits the mount source, which is a persistence guarantee and not a confidentiality one, since anything running in that RUN can read the mounted paths.

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
COPY mount/ /fragment/mount/
COPY hooks/ /fragment/hooks/
```

No `RUN` commands, no base image. The fragment carries only the files needed for assembly. Omit the `tree/`, `mount/`, or `hooks/` line if the fragment has no such directory.

Each directory needs its own `COPY` with an explicit destination. A single `COPY fragment.toml tree/ hooks/ /fragment/` copies the *contents* of `tree/` and `hooks/` into `/fragment/`, so neither `/fragment/tree/` nor `/fragment/hooks/` exists and the fragment reads as empty. That form builds and pushes without error, so the mistake surfaces only when the tool loads the fragment.

Build and push:
```bash
podman build -f Containerfile.fragment -t quay.io/user/fragment:1.0 .
podman push quay.io/user/fragment:1.0
```

## OCI Annotations (Fast-Path Metadata)

For performance, fragments should include OCI annotations that mirror `fragment.toml` fields. When present, `list` can read metadata without pulling layers; `inspect` reports tree and hook contents, so it always pulls the layer.

Annotation keys:
- `com.github.marrusl.osfragment.name`: fragment name
- `com.github.marrusl.osfragment.version`: version string
- `com.github.marrusl.osfragment.description`: description text
- `com.github.marrusl.osfragment.vendor`: vendor name (optional)
- `com.github.marrusl.osfragment.provides.repos`: JSON array of repo IDs (e.g., `["epel"]`)
- `com.github.marrusl.osfragment.packages.required`: JSON array of required package names
- `com.github.marrusl.osfragment.mounts`: JSON array of mount target paths (e.g., `["/etc/pki/entitlement"]`)

`com.github.marrusl.osfragment.mounts` has no `fragment.toml` counterpart: its authority is the derived targets, so generation cross-checks it whenever it pulls the layer and warns on drift, with layer content winning. Annotating buys this: `list` answers the mount question from registry metadata only when this key is present, and falls back to a full layer pull when it is absent, because metadata alone cannot tell a fragment that mounts nothing from one that never annotated. To set it, run `inspect` on the local fragment directory to see the derived targets, then pass them as `--annotation` on your own `podman build`.

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

fragments:
  - image: quay.io/example/epel:10
    packages: [htop, tmux]
  - image: quay.io/example/tailscale:1.82.0
    packages: [tailscale]
    mirror: https://mirror.internal.corp/tailscale
```

### Fields

- `apiVersion`: Must be `osfragment/v1alpha1`
- `kind`: Must be `Composition`
- `base`: Required. Base bootc image reference (RHCOS included). The base is never probed to
  decide behavior; the generated Containerfile validates it at build time via
  `bootc container lint`. Only `--pin-digests` contacts the registry for the base, to resolve
  its digest.
- `fragments`: Required. Array of at least one fragment.
  - `image`: Required. OCI image reference (`registry/repo:tag`) or digest (`registry/repo@sha256:...`).
  - `packages`: Optional. Array of package names to install from this fragment's repos. Defaults to `[]`.
  - `mirror`: Optional. A base URL to rewrite this fragment's `.repo` files against: `baseurl` is replaced with the given URL, and any `metalink`/`mirrorlist` lines are commented out (e.g., `https://satellite.corp/repo`).

Unknown manifest keys are rejected as parse errors, at the top level and inside fragment entries, so a misspelled field fails the parse instead of being silently ignored.

Package installation is deduplicated across all fragments; if multiple fragments request the same package, it's installed once.

Repo deduplication: If multiple fragments provide `.repo` files with the same filename, the tool compares their content. Identical content is silently deduplicated (last fragment wins). Different content for the same repo ID causes the build to fail.
