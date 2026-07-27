# bootc-assemble

A tool for building composable bootc and RHCOS images from fragment OCI images.

## What it does

bootc-assemble reads a YAML manifest declaring a base bootc/RHCOS image and a set of fragment OCI images, then generates a multi-stage Containerfile. Fragments are standard OCI images that package repo configs, RPM GPG keys, config files, systemd presets, and scripts into reusable units. The tool handles ordering (files before packages, packages before scripts), deduplication (identical repo definitions from multiple fragments), and optionally pins all references to content-addressed digests.

## Getting Started

### Prerequisites

- Rust toolchain (1.70 or later)
- skopeo
- podman

### Build

```bash
cargo build --release
```

The binary will be at `target/release/bootc-assemble`.

### Inspect a fragment

Examine a local fragment's metadata and contents:

```bash
./target/release/bootc-assemble inspect examples/fragments/tailscale
```

This shows the fragment's structure, metadata from `fragment.toml`, and files in the `tree/` directory.

### Build and push fragment images

Each fragment directory contains a `Containerfile.fragment`. Build and push them to a registry:

```bash
# Start a local registry (for testing)
podman run -d -p 5050:5000 --name registry docker.io/library/registry:2

# Build and push a fragment
cd examples/fragments/epel
podman build -f Containerfile.fragment -t localhost:5050/fragments/epel:10 .
podman push localhost:5050/fragments/epel:10

# Repeat for other fragments
cd ../tailscale
podman build -f Containerfile.fragment -t localhost:5050/fragments/tailscale:1.82.0 .
podman push localhost:5050/fragments/tailscale:1.82.0

cd ../grafana
podman build -f Containerfile.fragment -t localhost:5050/fragments/grafana:11.0 .
podman push localhost:5050/fragments/grafana:11.0

cd ../postgresql
podman build -f Containerfile.fragment -t localhost:5050/fragments/postgresql:17 .
podman push localhost:5050/fragments/postgresql:17

cd ../hashicorp
podman build -f Containerfile.fragment -t localhost:5050/fragments/hashicorp:1.0 .
podman push localhost:5050/fragments/hashicorp:1.0

cd ../cis-hardening
podman build -f Containerfile.fragment -t localhost:5050/fragments/cis-hardening:2.1 .
podman push localhost:5050/fragments/cis-hardening:2.1

cd ../node-exporter
podman build -f Containerfile.fragment -t localhost:5050/fragments/node-exporter:1.8.0 .
podman push localhost:5050/fragments/node-exporter:1.8.0

cd ../nginx
podman build -f Containerfile.fragment -t localhost:5050/fragments/nginx:1.26 .
podman push localhost:5050/fragments/nginx:1.26
```

For production use, push to quay.io or another public registry:

```bash
podman build -f Containerfile.fragment -t quay.io/your-username/fragments/epel:10 .
podman push quay.io/your-username/fragments/epel:10
```

### Update the manifest

Edit `examples/manifests/full.yaml` to point at your registry:

```yaml
apiVersion: bootc.io/v1alpha1
kind: Composition

base: quay.io/centos-bootc/centos-bootc:stream10

fragments:
  - image: localhost:5050/fragments/epel:10
    packages: [htop, tmux, jq]
  - image: localhost:5050/fragments/tailscale:1.82.0
    packages: [tailscale]
  - image: localhost:5050/fragments/grafana:11.0
    packages: [grafana]
  - image: localhost:5050/fragments/postgresql:17
    packages: [postgresql17-server]
  - image: localhost:5050/fragments/hashicorp:1.0
    packages: [vault]
  - image: localhost:5050/fragments/cis-hardening:2.1
  - image: localhost:5050/fragments/node-exporter:1.8.0
    packages: [node-exporter]
  - image: localhost:5050/fragments/nginx:1.26
    packages: [nginx]
```

### Generate the Containerfile

```bash
./target/release/bootc-assemble --manifest examples/manifests/full.yaml --output Containerfile
```

This reads the manifest and generates a multi-stage Containerfile that:
1. Extracts files from each fragment's `tree/` directory
2. Installs packages in the correct order
3. Runs scripts from each fragment
4. Deduplicates identical repo files

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

## CLI commands

### Generate a Containerfile

```bash
bootc-assemble [OPTIONS]
```

Options:
- `--manifest <path>` - Path to manifest file (default: `bootc-assemble.yaml`)
- `--output <path>` - Output Containerfile path (default: `Containerfile`)
- `--pin-digests` - Resolve and pin all image refs to sha256 digests

### Inspect a fragment

```bash
bootc-assemble inspect <image-or-directory>
```

Examine a fragment's metadata and contents. Accepts either a local directory path or an OCI image reference.

### List fragments in a manifest

```bash
bootc-assemble list --manifest <path>
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
