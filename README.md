# osfragment-assemble

A tool for building composable bootc-compatible OS images from fragment OCI images.

## What it does

osfragment-assemble reads a YAML manifest declaring a base bootc-compatible image and a set of fragment OCI images, then generates a multi-stage Containerfile. Fragments are standard OCI images that package repo configs, RPM GPG keys, config files, systemd presets, and scripts into reusable units. The tool handles ordering (repo files before packages, packages before config files, config files before scripts), deduplication (identical repo definitions from multiple fragments), and optionally pins all references to content-addressed digests.

## Getting Started

### Prerequisites

- Rust toolchain (1.70 or later)
- skopeo (must be installed and authenticated to pull from registries)
- podman

### Build

```bash
cargo build --release
```

The binary will be at `target/release/osfragment`.

### Inspect a fragment

Examine a local fragment's metadata and contents:

```bash
./target/release/osfragment-assemble inspect examples/fragments/tailscale
```

### Generate a Containerfile

Pre-built fragment images are available at `quay.io/marrusl2/fragments/`. Generate a Containerfile using them:

```bash
./target/release/osfragment-assemble --manifest examples/manifests/full.yaml --output Containerfile
```

The manifest at `examples/manifests/full.yaml` already points to these public images, so no editing is needed.

### Build the final image

```bash
podman build -f Containerfile -t my-bootc-image:latest .
```

The generated Containerfile is a draft. Review and edit it before building for production use.

## Fragment structure

A fragment is a directory containing:

```
my-fragment/
├── Containerfile.fragment    # Builds the fragment OCI image
├── fragment.toml             # Metadata (name, version, description)
├── tree/                     # Files to copy into the base image
│   ├── etc/yum.repos.d/my.repo
│   └── etc/pki/rpm-gpg/RPM-GPG-KEY-my
└── scripts/                  # Scripts to run after package installation
    └── configure.sh
```

The `tree/` directory mirrors the target filesystem layout. Files are copied verbatim.

The `scripts/` directory contains shell scripts executed after package installation.

## Building your own fragments

Fragment images are standard OCI images. Each fragment directory contains a `Containerfile.fragment` that builds it:

```dockerfile
FROM scratch
COPY fragment.toml tree/ scripts/ /fragment/
```

Build and push to a registry:

```bash
cd my-fragment
podman build -f Containerfile.fragment -t quay.io/your-username/my-fragment:1.0 .
podman push quay.io/your-username/my-fragment:1.0
```

Then reference it in your manifest:

```yaml
fragments:
  - image: quay.io/your-username/my-fragment:1.0
    packages: [my-package]
```

See `examples/fragments/` for ready-to-use examples.

## CLI

### Generate a Containerfile

```bash
osfragment-assemble [OPTIONS]
```

- `--manifest <path>` — Path to manifest file (default: `osfragment-assemble.yaml`)
- `--output <path>` — Output Containerfile path (default: `Containerfile`)
- `--pin-digests` — Resolve and pin all image refs to sha256 digests
- `--ocp [<path>]` — Generate a MachineOSConfig YAML for OpenShift (default: `machineosbuild.yaml`)
- `--pool <name>` — MachineConfigPool name for `--ocp` output (default: `worker`)

### Inspect a fragment

```bash
osfragment-assemble inspect <image-or-directory>
```

Examine a fragment's metadata and contents. Accepts a local directory path or an OCI image reference.

### List fragments in a manifest

```bash
osfragment-assemble list --manifest <path>
```

List fragments in phase-sorted order (the order they'll appear in the generated Containerfile).

## Example fragments

The `examples/fragments/` directory contains 8 ready-to-use fragments:

- **epel** - EPEL repository configuration
- **tailscale** - Tailscale VPN with systemd preset
- **grafana** - Grafana repository and GPG key
- **postgresql** - PostgreSQL 17 repository
- **hashicorp** - HashiCorp repository (Vault, Terraform, etc.)
- **cis-hardening** - CIS security hardening configurations
- **node-exporter** - Prometheus Node Exporter
- **nginx** - nginx web server

## License

MIT - see LICENSE file.
