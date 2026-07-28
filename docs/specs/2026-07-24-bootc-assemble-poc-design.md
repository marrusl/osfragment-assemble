# osfragment-assemble POC Design Spec

## Overview

`osfragment-assemble` is a CLI tool that reads a YAML manifest of fragment OCI images and generates a multi-stage Containerfile for building bootc/RHCOS images. Fragments are a packaging convention — standard OCI images carrying any combination of repo configs, hooks, config files, systemd units, and filesystem overlays. The tool is codegen on top of existing build tooling: it generates Containerfiles that buildah/podman build consumes. No new image format, no new builder, no new package manager.

**Language:** Rust
**Repository:** `~/Work/osfragment`
**Audience:** Engineering peers first (Colin Walters, Giuseppe Scrivano, bootc maintainers), product/strategy stakeholders second.

## Fragment Image Anatomy

A fragment is a standard OCI image with this internal structure:

```
/fragment/
  fragment.toml          # metadata
  tree/                  # files overlaid into the image
  hooks/                 # optional post-install executables (any language)
    configure.sh         # executed in alphabetical order
```

There is no type taxonomy. A fragment's content determines what it is — a lightweight repo connector carries only `tree/etc/yum.repos.d/` and a GPG key. An opinionated capability fragment carries repo config, default configuration, hooks, and systemd units. Same format, same tooling.

### Hooks Contract

- **All files executed:** All executable files in `hooks/` are run in alphabetical order after package installation. Fragment authors are responsible for ensuring files are executable and that any required interpreters are available in the image at build time.
- **Configuration only.** Hooks should not call `dnf`, `rpm`, or otherwise mutate package state. The manifest-driven `dnf install` is the sole package installation mechanism. This preserves the package-management firewall: the manifest declares intent, dnf resolves, and hooks configure what was installed.
- **Runs as root** in the build context after packages are installed. See Trust Boundary for implications.

### fragment.toml

```toml
[fragment]
name = "tailscale"
version = "1.82.0"
description = "Tailscale VPN client — repo, packages, and service enablement"
vendor = "Tailscale Inc."
phase = "config"

[fragment.provides]
repos = ["tailscale-stable"]

[fragment.packages]
available = ["tailscale"]

[fragment.conflicts]
fragments = []
```

**Required fields:** `name`, `version`, `description`, `phase`.
**Optional fields:** `vendor`, `provides.repos`, `packages.available`, `conflicts.fragments`.

### OCI Annotations (Fast Path)

Fragment metadata is also published as OCI annotations on the image manifest, enabling metadata reads without layer extraction:

- `io.bootc.fragment.name`
- `io.bootc.fragment.version`
- `io.bootc.fragment.phase`
- `io.bootc.fragment.provides.repos` (JSON array)
- `io.bootc.fragment.packages.available` (JSON array)

The tool checks annotations first via `skopeo inspect --raw`. Falls back to layer extraction only when annotations are absent.

## Trust Boundary

Fragments are trusted build-time code, not passive data. A fragment's `tree/` content is copied into the image with root ownership, and its hooks run as root during the build. A malicious or compromised fragment has the same effective power as arbitrary `RUN` instructions in the Containerfile.

For the POC, the supported trust model is: **self-authored or allowlisted fragments from controlled registries only.** Consuming arbitrary third-party fragments from public registries is out of scope until signature verification (cosign) or policy enforcement exists.

Digest pinning (see OCI Image Consumption) provides integrity — the build consumes exactly what was inspected — but not provenance. Provenance verification is a post-POC concern.

The distinction between fragment provenance (a build-input supply-chain problem) and runtime image integrity (the composefs/fs-verity chain) is intentional. The assembled image participates in the sealed-image runtime story normally; fragment trust is about what goes *into* the build, not what comes *out*.

## Filesystem Authoring Guidance

Fragment authors should follow bootc filesystem conventions:

- **Prefer `/usr` and `/usr/lib` for immutable image payload.** Vendor defaults, configuration drop-ins, and systemd units belong in `/usr/lib/` (e.g., `/usr/lib/sysctl.d/`, `/usr/lib/systemd/system/`, `/usr/lib/tmpfiles.d/`). This content is part of the immutable image and survives upgrades cleanly.
- **Treat `/etc` as an exceptional compatibility surface.** `/etc` is mutable state at runtime. Content placed there at build time may be overwritten by local admin changes or 3-way merge behavior on upgrade. Use `/etc` only when the software being configured requires it (e.g., `/etc/yum.repos.d/` for repo definitions, `/etc/pki/rpm-gpg/` for GPG keys). Upstream bootc is moving toward transient `/etc` on the composefs path — fragments that lean on `/etc` for vendor defaults will have increasing friction.
- **Service enablement via presets.** Use `/usr/lib/systemd/system-preset/` files to enable services rather than running `systemctl enable` in hooks. Preset files are immutable image content and compose cleanly across fragments.

The `lint` subcommand (future phase) will warn on `/etc` writes that have `/usr/lib` equivalents.

## Phase System

Phases control ordering in the generated Containerfile. Fragments declare one of two phases. The tool manages the other two steps.

| Phase | Weight | Owner | Purpose |
|-------|--------|-------|---------|
| `repos` | 10 | Fragment-declared | `.repo` files, GPG keys land before any `dnf install` |
| *(packages)* | 20 | Tool-managed | `dnf install` — batched from all manifest `packages:` fields |
| `config` | 30 | Fragment-declared | Post-install: config files, preset files, hooks, sysctl, SELinux |
| *(preset-apply)* | 35 | Tool-managed | `systemctl preset-all` — applies preset policy to enable/disable services |
| *(validation)* | 90 | Tool-managed | `bootc container lint` |

Weights 40–89 are reserved for future phases. Within a phase, manifest ordering controls sequencing (first listed = first applied).

### Preset Application (Weight 35)

Preset files (`/usr/lib/systemd/system-preset/`) are policy declarations, not enablement by themselves. After all `config`-phase fragments have placed their preset files and service units, the tool emits `RUN systemctl preset-all` to apply the accumulated preset policy. This is what actually creates the symlinks in `/etc/systemd/system/` that enable services at boot. Running this as a single tool-managed step after all fragments ensures preset policy from multiple fragments composes correctly.

### Tree-Splitting Rule

A fragment's `tree/` content is split across phases based on path, not the fragment's declared phase:

- **Repos phase (10):** The tool copies `tree/etc/yum.repos.d/` and `tree/etc/pki/rpm-gpg/` from **all** fragments that contain them, regardless of the fragment's declared phase. This ensures repo definitions are available before `dnf install`.
- **Config phase (30):** The tool copies the remainder of `tree/` — everything outside `tree/etc/yum.repos.d/` and `tree/etc/pki/rpm-gpg/`. For fragments that only carry repo files, this step is a no-op.

This means a `config` fragment like Tailscale that carries both repo files and `/usr/lib/` preset files works naturally: its repo files land at weight 10, packages install at weight 20, and its preset files and other config land at weight 30.

### Phase Consistency Validation

- A `repos` fragment must contain only repo-related `tree/` content (`tree/etc/yum.repos.d/`, `tree/etc/pki/rpm-gpg/`). It must not carry hooks or non-repo `tree/` paths. If it does, the tool fails with an error suggesting the fragment's phase should be `config`.
- A `config` fragment may carry any `tree/` content including repo files. Repo files are still split to the repos phase automatically.

## Manifest Format

```yaml
# osfragment-assemble.yaml
apiVersion: bootc.io/v1alpha1
kind: Composition

base: registry.redhat.io/rhel10/rhel-bootc:10.0

fragments:
  - image: quay.io/marrusl2/fragments/epel:10
    packages: [htop, tmux]

  - image: quay.io/marrusl2/fragments/tailscale:1.82.0
    packages: [tailscale]

  - image: quay.io/marrusl2/fragments/grafana:11.0
    packages: [grafana]
    mirror: https://rpm-mirror.internal.corp/grafana/

  - image: quay.io/marrusl2/fragments/cis-hardening:2.1
```

### Manifest Fields

- **`base`** (required): Base bootc image reference.
- **`fragments`** (required): List of fragment entries.
  - **`image`** (required): Fragment OCI image reference, or `dir:./local-path` for local directory mode.
  - **`packages`** (optional): Packages to install. Listed per-fragment for organizational clarity (documenting which packages relate to which fragment), but all selections across all fragments are batched into a single `dnf install` transaction with all repos enabled. dnf resolves from whichever repo provides the package — the per-fragment grouping is intent documentation, not an enforcement boundary.
  - **`mirror`** (optional): Override the fragment's `.repo` `baseurl` with this URL. Disables `mirrorlist`/`metalink` if present. Per-fragment, not global.

### Repo Deduplication

Fragments are self-contained — each carries everything it needs to work, including repo definitions. When multiple fragments declare `provides.repos` with the same repo ID, the tool deduplicates: one `COPY` of the repo files in the output. If two fragments provide the same repo ID but with conflicting definitions, the build fails with a clear error.

## CLI Design

```
osfragment-assemble [--manifest osfragment-assemble.yaml] [--output Containerfile]
osfragment-assemble inspect <fragment-image-or-directory>
osfragment-assemble list [--manifest osfragment-assemble.yaml]
```

### Default Command (Assembly)

No subcommand needed — the tool name is the verb.

1. Parse manifest
2. Pull fragment metadata via skopeo
3. Read `fragment.toml` from each fragment (layer extraction; annotations fast path for inspect/list only)
4. Validate: declared conflicts, phase consistency, repo deduplication
5. Sort fragments by phase weight, preserve manifest order within phases
6. Compute override summary — detect file path collisions between fragments
7. Generate Containerfile with override summary as header comment
8. Write to `--output` path (default: `./Containerfile`)
**Flags:**
- `--manifest <path>`: Manifest file (default: `./osfragment-assemble.yaml`)
- `--output <path>`: Containerfile output path (default: `./Containerfile`)

> **Deferred:** `--build` (run `podman build` after generation) and `--local` (local directory assembly mode) were removed from the POC. The generated Containerfile is a draft — users edit it before building with `podman build -f Containerfile .` directly.

### `inspect`

Point at a registry image or local directory. Display `fragment.toml` metadata, list `tree/` contents, list hooks.

```
$ osfragment-assemble inspect quay.io/marrusl2/fragments/tailscale:1.82.0

Fragment: tailscale v1.82.0
Vendor:   Tailscale Inc.
Phase:    config
Repos:    tailscale-stable
Packages: tailscale

tree/
  etc/yum.repos.d/tailscale.repo
  etc/pki/rpm-gpg/RPM-GPG-KEY-tailscale
  usr/lib/systemd/system-preset/50-tailscale.preset

hooks/
  configure.sh (present)
```

### `list`

Show fragments from the manifest in phase-sorted order.

```
$ osfragment-assemble list

Manifest: osfragment-assemble.yaml
Base:     registry.redhat.io/rhel10/rhel-bootc:10.0

  NAME               PHASE    VERSION    PACKAGES
  epel               repos    10         htop, tmux
  tailscale          config   1.82.0     tailscale
  grafana            config   11.0       grafana
  cis-hardening      config   2.1        —

4 fragments
```

## OCI Image Consumption

### Registry Mode

The tool shells out to `skopeo` for all registry interaction:

- **Metadata fast path:** `skopeo inspect --raw docker://<image>` — reads OCI annotations and the manifest digest (single HTTP GET, no layer pull).
- **Layer extraction fallback:** `skopeo copy docker://<image> oci:<tmp-dir>` — copies to OCI layout on disk. Parse `index.json` to find the manifest, locate the single layer blob, and stream-extract `fragment.toml` from the tarball at its known path (`/fragment/fragment.toml`) using `flate2`/`tar` crates. See Layer Extraction Contract below.
- **Digest resolution:** During metadata read, the tool resolves every fragment (and the base image) to an immutable digest. The generated Containerfile emits `FROM <image>@sha256:... AS frag-<name>` with the human-friendly tag as an inline comment. This closes the TOCTOU gap between inspection and build — the build consumes exactly what was validated. The `list` output and generated header comment surface the resolved digests.

skopeo is the right choice for a POC: it handles auth, mirrors, token exchange, and transport edge cases via `containers/image`. The audience already has it installed. The interaction is behind a module boundary so native Rust OCI can replace it later if this graduates from POC.

### Layer Extraction Contract

When extracting `fragment.toml` from a layer tarball, the tool follows a fail-closed parser contract:

- **Stream-only extraction.** Never unpack layers to disk. Stream tar entries and read only the exact `/fragment/fragment.toml` path.
- **Reject traversal and link tricks.** Reject entries with `..` path components, absolute paths outside `/fragment/`, symlinks, and hardlinks.
- **Cap metadata size.** Reject `fragment.toml` entries larger than 64 KB (well beyond any reasonable metadata).
- **Fail on ambiguity.** If multiple entries claim the `/fragment/fragment.toml` path, fail with an error rather than picking one.

### Local Directory Mode (Deferred)

Local directory assembly (`dir:` prefix, `--local` flag) was removed from the POC — the prebuild/cleanup complexity was disproportionate to its value. Fragment development workflow: build the fragment image with `podman build`, push to a registry, then reference it in the manifest.

The `inspect` command retains local directory support (`osfragment-assemble inspect ./my-fragment/`) for examining fragments during development — this reads files directly with no prebuild infrastructure.

## Generated Containerfile

Example output for a manifest with EPEL, Tailscale, and CIS Hardening:

```dockerfile
# Generated by osfragment-assemble v0.1.0
# Manifest: osfragment-assemble.yaml
# Fragments: epel (repos), tailscale (config), cis-hardening (config)
# Resolved digests:
#   base: registry.redhat.io/rhel10/rhel-bootc:10.0@sha256:a1b2c3...
#   epel: quay.io/marrusl2/fragments/epel:10@sha256:d4e5f6...
#   tailscale: quay.io/marrusl2/fragments/tailscale:1.82.0@sha256:789abc...
#   cis-hardening: quay.io/marrusl2/fragments/cis-hardening:2.1@sha256:def012...
# Override summary: no file path collisions detected

# --- Fragment stages ---
FROM quay.io/marrusl2/fragments/epel@sha256:d4e5f6... AS frag-epel              # :10
FROM quay.io/marrusl2/fragments/tailscale@sha256:789abc... AS frag-tailscale     # :1.82.0
FROM quay.io/marrusl2/fragments/cis-hardening@sha256:def012... AS frag-cis-hardening  # :2.1

# --- Base ---
FROM registry.redhat.io/rhel10/rhel-bootc@sha256:a1b2c3...                       # :10.0

# --- Phase: repos (10) ---
COPY --from=frag-epel /fragment/tree/etc/yum.repos.d/ /etc/yum.repos.d/
COPY --from=frag-epel /fragment/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/
COPY --from=frag-tailscale /fragment/tree/etc/yum.repos.d/ /etc/yum.repos.d/
COPY --from=frag-tailscale /fragment/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/

# --- Phase: packages (20) ---
RUN dnf install -y \
        htop \
        tmux \
        tailscale \
    && dnf clean all

# --- Phase: config (30) ---
# tailscale — non-repo tree content (repo files already copied above)
COPY --from=frag-tailscale /fragment/tree/usr/ /usr/
COPY --from=frag-tailscale /fragment/hooks/ /tmp/frag-tailscale-hooks/
RUN /tmp/frag-tailscale-hooks/configure.sh && rm -rf /tmp/frag-tailscale-hooks

# cis-hardening — all tree content (no repo files in this fragment)
COPY --from=frag-cis-hardening /fragment/tree/ /
COPY --from=frag-cis-hardening /fragment/hooks/ /tmp/frag-cis-hardening-hooks/
RUN /tmp/frag-cis-hardening-hooks/configure.sh && rm -rf /tmp/frag-cis-hardening-hooks

# --- Phase: preset-apply (35) ---
RUN systemctl preset-all --preset-mode=enable-only 2>/dev/null || true

# --- Phase: validation (90) ---
RUN bootc container lint
```

### Generation Rules

- All fragment and base image references are resolved to digests during metadata read. The generated Containerfile uses `<image>@sha256:...` with the original tag as an inline comment.
- All fragment images become `FROM <image>@sha256:... AS frag-<name>` stages at the top.
- **Tree-splitting:** Repo files (`tree/etc/yum.repos.d/`, `tree/etc/pki/rpm-gpg/`) from all fragments are copied in the repos phase (weight 10), regardless of the fragment's declared phase. Remaining `tree/` content is copied in the config phase (weight 30), excluding the repo paths already copied. For fragments whose `tree/` contains only repo files, the config phase copy is a no-op.
- All `packages:` selections across all fragments are batched into a single `dnf install` transaction with all repos enabled.
- If a fragment has hooks, the entire `hooks/` directory is copied to a temp path and all files are executed in alphabetical order: `COPY --from=frag-<name> /fragment/hooks/ /tmp/frag-<name>-hooks/` then `RUN /tmp/frag-<name>-hooks/configure.sh && rm -rf /tmp/frag-<name>-hooks`. Fragments without hooks skip this step.
- **Preset application:** After all config-phase content is in place, the tool emits `RUN systemctl preset-all --preset-mode=enable-only 2>/dev/null || true` (weight 35). This applies accumulated preset policy from all fragments, creating the enablement symlinks that make services start at boot. The `--preset-mode=enable-only` flag avoids disabling services the base image already enabled. The `|| true` handles images without systemd.
- `mirror:` rewrites `baseurl` in the copied `.repo` file via a `RUN sed` after the `COPY`.
- Local directory fragments are prebuilt to temporary local images before codegen (see Local Directory Mode).
- `bootc container lint` is always the final step.

## Example Fragments

8 fragments using freely available content:

| # | Name | Repo Source | Phase | Content | Demo Value |
|---|------|------------|-------|---------|------------|
| 1 | **epel** | `dl.fedoraproject.org` | `repos` | `.repo` + GPG key | Lightest possible fragment — pure repo connector |
| 2 | **tailscale** | `pkgs.tailscale.com` | `config` | Repo + package + systemd enable | Full spectrum in one fragment — proves unified model |
| 3 | **grafana** | `rpm.grafana.com` | `config` | Repo + package + default config + service enable | Monitoring story, `mirror:` override demo |
| 4 | **postgresql** | `yum.postgresql.org` | `repos` | Repo + GPG key, multi-version streams | Customer picks `postgresql16-server` vs `postgresql17-server` |
| 5 | **hashicorp** | `rpm.releases.hashicorp.com` | `config` | Repo + Vault package + basic config | Infrastructure tooling on image-mode |
| 6 | **cis-hardening** | *(none)* | `config` | Sysctl, audit rules, fs permissions | Pure config overlay — no repo, other end of spectrum |
| 7 | **node-exporter** | EPEL (self-contained) | `config` | EPEL repo + GPG (own copy) + package + systemd + scrape config | Self-contained — carries own EPEL repo, dedup demo |
| 8 | **nginx** | `nginx.org/packages` | `config` | Repo + package + config + service enable | Relatable web server use case |

Each fragment is built as an OCI image via a simple `Containerfile.fragment` that copies `fragment.toml`, `tree/`, and `hooks/` into `/fragment/`. Published to local dev registry during development, quay.io for demos.

### Fragment Build Pattern

Fragment images must be single-layer. This simplifies layer extraction — one blob to find and stream. The build pattern uses a single `COPY` instruction:

```dockerfile
FROM scratch
COPY fragment.toml tree/ hooks/ /fragment/
```

This produces one layer containing all fragment content at `/fragment/`. The tool's layer extraction assumes a single layer per fragment image and fails if multiple layers are found.

## Out of Scope for POC

- `lint` and `validate` subcommands (follow-up phase)
- `--target ocp` / MachineOSConfig emission
- `tls-ca:` manifest field
- `ocp:` manifest block
- Composition stacking validation (works naturally — out-of-scope to test)
- cosign signature verification of fragments
- Fragment discovery / catalog

## Project Structure

```
osfragment/
  Cargo.toml
  src/
    main.rs                 # CLI entry point (clap)
    manifest.rs             # YAML manifest parsing
    fragment.rs             # fragment.toml parsing, FragmentSource enum
    registry.rs             # skopeo interaction, OCI annotation reading
    generator.rs            # Containerfile generation engine
    inspect.rs              # inspect subcommand
    list.rs                 # list subcommand
  examples/
    fragments/
      epel/
        fragment.toml
        tree/
        Containerfile.fragment
      tailscale/
      grafana/
      postgresql/
      hashicorp/
      cis-hardening/
      node-exporter/
      nginx/
    manifests/
      minimal.yaml          # 2 fragments
      full.yaml             # all 8
      mirror-demo.yaml      # shows mirror: override
  docs/
    specs/
```

## Success Criteria

1. `osfragment-assemble` reads a manifest and generates a correct multi-stage Containerfile
2. Generated Containerfile builds successfully with `podman build`
3. `inspect` shows fragment metadata from both registry and local directory sources
4. `list` shows phase-sorted fragment summary from a manifest
5. At least 4 of 8 example fragments work end-to-end (build fragment → push → assemble → build image)
6. `mirror:` override produces correct `.repo` rewrite in generated Containerfile
7. Repo deduplication works when two fragments provide the same repo ID
8. The demo tells a coherent story: "here's the problem, here are fragments, here's what the tool generates, here's the built image"
