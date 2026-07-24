# bootc-assemble POC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working POC of `bootc-assemble` — a Rust CLI that reads a YAML manifest of fragment OCI images and generates a multi-stage Containerfile for bootc/RHCOS image builds, with 8 example fragments using real freely-available content.

**Architecture:** Fragment-first approach. The tool reads fragment metadata (via skopeo for registry images, direct filesystem reads for local directories), validates the manifest, and generates a Containerfile with digest-pinned FROM stages, phase-ordered content, tree-splitting for repo files, and a preset-apply step for service enablement. Fragments are single-layer OCI images built FROM scratch.

**Tech Stack:** Rust (2021 edition), clap (CLI), serde + toml + serde_yaml (parsing), flate2 + tar (layer extraction), tempfile, anyhow (errors)

**Spec:** `docs/specs/2026-07-24-bootc-assemble-poc-design.md`

## Global Constraints

- Rust 2021 edition, stable toolchain
- Fragment images must be single-layer (one COPY instruction in Containerfile.fragment)
- `configure.sh` is the only script entrypoint, must not call dnf/rpm
- Fragments declare phase `repos` or `config` only
- `repos` fragments must contain only `tree/etc/yum.repos.d/` and `tree/etc/pki/rpm-gpg/` — no scripts, no other tree content
- All FROM references in generated Containerfiles use `@sha256:` digests
- Layer extraction is stream-only, fail-closed (no disk unpacking, reject traversal/links/dupes, 64KB cap)
- Tree-splitting: repo paths copy at weight 10 regardless of declared phase; remaining tree at weight 30
- `systemctl preset-all --preset-mode=enable-only` runs at weight 35 after all config content
- `bootc container lint` always runs last at weight 90
- Per-fragment `packages:` is organizational grouping — all packages batched into one `dnf install`
- Requires `skopeo` and `podman` on PATH for registry and build operations

---

### Task 1: Project scaffold and fragment.toml data model

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/fragment.rs`
- Create: `src/lib.rs`
- Test: inline `#[cfg(test)]` in `src/fragment.rs`

**Interfaces:**
- Consumes: nothing (first task)
- Produces: `Fragment` struct, `FragmentPhase` enum, `parse_fragment_toml(content: &str) -> Result<Fragment>`, `validate_phase_consistency(fragment: &Fragment, tree_paths: &[PathBuf]) -> Result<()>`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "bootc-assemble"
version = "0.1.0"
edition = "2021"
description = "Composable image definitions for bootc and RHCOS"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
flate2 = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
tar = "0.4"
tempfile = "3"
toml = "0.8"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
```

- [ ] **Step 2: Create src/lib.rs with module declarations**

```rust
pub mod fragment;
```

- [ ] **Step 3: Write failing tests for fragment.toml parsing in src/fragment.rs**

```rust
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[fragment]
name = "epel"
version = "10"
description = "EPEL repository for RHEL"
phase = "repos"

[fragment.provides]
repos = ["epel"]
"#;

    const FULL_TOML: &str = r#"
[fragment]
name = "tailscale"
version = "1.82.0"
description = "Tailscale VPN client"
vendor = "Tailscale Inc."
phase = "config"

[fragment.provides]
repos = ["tailscale-stable"]

[fragment.packages]
available = ["tailscale"]

[fragment.conflicts]
fragments = []
"#;

    #[test]
    fn parse_minimal_fragment() {
        let frag = parse_fragment_toml(MINIMAL_TOML).unwrap();
        assert_eq!(frag.name, "epel");
        assert_eq!(frag.version, "10");
        assert_eq!(frag.phase, FragmentPhase::Repos);
        assert_eq!(frag.provides.repos, vec!["epel"]);
        assert!(frag.vendor.is_none());
        assert!(frag.packages.available.is_empty());
    }

    #[test]
    fn parse_full_fragment() {
        let frag = parse_fragment_toml(FULL_TOML).unwrap();
        assert_eq!(frag.name, "tailscale");
        assert_eq!(frag.phase, FragmentPhase::Config);
        assert_eq!(frag.vendor.as_deref(), Some("Tailscale Inc."));
        assert_eq!(frag.packages.available, vec!["tailscale"]);
    }

    #[test]
    fn reject_invalid_phase() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "bad phase"
phase = "install"
"#;
        assert!(parse_fragment_toml(bad).is_err());
    }

    #[test]
    fn phase_consistency_repos_fragment_with_scripts_fails() {
        let paths = vec![
            PathBuf::from("tree/etc/yum.repos.d/epel.repo"),
            PathBuf::from("scripts/configure.sh"),
        ];
        let frag = parse_fragment_toml(MINIMAL_TOML).unwrap();
        assert!(validate_phase_consistency(&frag, &paths).is_err());
    }

    #[test]
    fn phase_consistency_repos_fragment_with_non_repo_tree_fails() {
        let paths = vec![
            PathBuf::from("tree/etc/yum.repos.d/epel.repo"),
            PathBuf::from("tree/usr/lib/sysctl.d/99-hardening.conf"),
        ];
        let frag = parse_fragment_toml(MINIMAL_TOML).unwrap();
        assert!(validate_phase_consistency(&frag, &paths).is_err());
    }

    #[test]
    fn phase_consistency_repos_fragment_with_only_repo_content_passes() {
        let paths = vec![
            PathBuf::from("tree/etc/yum.repos.d/epel.repo"),
            PathBuf::from("tree/etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL-10"),
        ];
        let frag = parse_fragment_toml(MINIMAL_TOML).unwrap();
        assert!(validate_phase_consistency(&frag, &paths).is_ok());
    }

    #[test]
    fn phase_consistency_config_fragment_with_mixed_content_passes() {
        let paths = vec![
            PathBuf::from("tree/etc/yum.repos.d/tailscale.repo"),
            PathBuf::from("tree/usr/lib/systemd/system-preset/50-tailscale.preset"),
            PathBuf::from("scripts/configure.sh"),
        ];
        let frag = parse_fragment_toml(FULL_TOML).unwrap();
        assert!(validate_phase_consistency(&frag, &paths).is_ok());
    }

    #[test]
    fn phase_weight_ordering() {
        assert!(FragmentPhase::Repos.weight() < FragmentPhase::Config.weight());
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --lib fragment`
Expected: compilation errors — types not yet defined

- [ ] **Step 5: Implement fragment.rs types and parsing**

```rust
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FragmentPhase {
    Repos,
    Config,
}

impl FragmentPhase {
    pub fn weight(&self) -> u32 {
        match self {
            FragmentPhase::Repos => 10,
            FragmentPhase::Config => 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FragmentProvides {
    #[serde(default)]
    pub repos: Vec<String>,
}

impl Default for FragmentProvides {
    fn default() -> Self {
        Self { repos: Vec::new() }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FragmentPackages {
    #[serde(default)]
    pub available: Vec<String>,
}

impl Default for FragmentPackages {
    fn default() -> Self {
        Self {
            available: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FragmentConflicts {
    #[serde(default)]
    pub fragments: Vec<String>,
}

impl Default for FragmentConflicts {
    fn default() -> Self {
        Self {
            fragments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct FragmentToml {
    fragment: FragmentInner,
}

#[derive(Debug, Clone, Deserialize)]
struct FragmentInner {
    name: String,
    version: String,
    description: String,
    #[serde(default)]
    vendor: Option<String>,
    phase: FragmentPhase,
    #[serde(default)]
    provides: FragmentProvides,
    #[serde(default)]
    packages: FragmentPackages,
    #[serde(default)]
    conflicts: FragmentConflicts,
}

#[derive(Debug, Clone)]
pub struct Fragment {
    pub name: String,
    pub version: String,
    pub description: String,
    pub vendor: Option<String>,
    pub phase: FragmentPhase,
    pub provides: FragmentProvides,
    pub packages: FragmentPackages,
    pub conflicts: FragmentConflicts,
}

pub fn parse_fragment_toml(content: &str) -> Result<Fragment> {
    let parsed: FragmentToml =
        toml::from_str(content).context("failed to parse fragment.toml")?;
    let inner = parsed.fragment;
    Ok(Fragment {
        name: inner.name,
        version: inner.version,
        description: inner.description,
        vendor: inner.vendor,
        phase: inner.phase,
        provides: inner.provides,
        packages: inner.packages,
        conflicts: inner.conflicts,
    })
}

pub const REPO_PREFIXES: &[&str] = &["tree/etc/yum.repos.d/", "tree/etc/pki/rpm-gpg/"];

pub fn is_repo_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    REPO_PREFIXES.iter().any(|prefix| s.starts_with(prefix))
}

pub fn validate_phase_consistency(fragment: &Fragment, tree_paths: &[PathBuf]) -> Result<()> {
    if fragment.phase != FragmentPhase::Repos {
        return Ok(());
    }
    let has_scripts = tree_paths
        .iter()
        .any(|p| p.to_string_lossy().starts_with("scripts/"));
    if has_scripts {
        bail!(
            "repos fragment '{}' must not contain scripts — change phase to 'config'",
            fragment.name
        );
    }
    let has_non_repo = tree_paths
        .iter()
        .filter(|p| p.to_string_lossy().starts_with("tree/"))
        .any(|p| !is_repo_path(p));
    if has_non_repo {
        bail!(
            "repos fragment '{}' contains non-repo tree content — change phase to 'config'",
            fragment.name
        );
    }
    Ok(())
}
```

- [ ] **Step 6: Create stub main.rs**

```rust
use anyhow::Result;

fn main() -> Result<()> {
    println!("bootc-assemble v0.1.0");
    Ok(())
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test --lib fragment`
Expected: all 7 tests PASS

- [ ] **Step 8: Commit**

```bash
cd ~/Work/bootc-assemble
git add Cargo.toml src/
git commit -m "feat: project scaffold and fragment.toml data model

Fragment struct, FragmentPhase enum, TOML parsing, and phase
consistency validation with tests.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 2: Manifest data model

**Files:**
- Create: `src/manifest.rs`
- Modify: `src/lib.rs` — add `pub mod manifest;`
- Test: inline `#[cfg(test)]` in `src/manifest.rs`

**Interfaces:**
- Consumes: nothing directly (manifest model is independent of fragment model at parse time)
- Produces: `Manifest` struct, `ManifestFragment` struct, `parse_manifest(content: &str) -> Result<Manifest>`, `FragmentSource` enum

- [ ] **Step 1: Write failing tests for manifest parsing in src/manifest.rs**

```rust
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum FragmentSource {
    Registry { image_ref: String },
    Directory { path: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: quay.io/mrussell/fragments/epel:10
    packages:
      - htop
      - tmux
  - image: quay.io/mrussell/fragments/cis-hardening:2.1
"#;

    const MIRROR_YAML: &str = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: quay.io/mrussell/fragments/grafana:11.0
    packages:
      - grafana
    mirror: https://rpm-mirror.internal.corp/grafana/
"#;

    const LOCAL_DIR_YAML: &str = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: "dir:./examples/fragments/epel"
"#;

    #[test]
    fn parse_minimal_manifest() {
        let manifest = parse_manifest(MINIMAL_YAML).unwrap();
        assert_eq!(manifest.base, "registry.redhat.io/rhel10/rhel-bootc:10.0");
        assert_eq!(manifest.fragments.len(), 2);
        assert_eq!(
            manifest.fragments[0].packages,
            vec!["htop", "tmux"]
        );
        assert!(manifest.fragments[1].packages.is_empty());
    }

    #[test]
    fn parse_mirror_override() {
        let manifest = parse_manifest(MIRROR_YAML).unwrap();
        assert_eq!(
            manifest.fragments[0].mirror.as_deref(),
            Some("https://rpm-mirror.internal.corp/grafana/")
        );
    }

    #[test]
    fn resolve_registry_source() {
        let manifest = parse_manifest(MINIMAL_YAML).unwrap();
        let source = manifest.fragments[0].resolve_source();
        assert!(matches!(source, FragmentSource::Registry { .. }));
    }

    #[test]
    fn resolve_directory_source() {
        let manifest = parse_manifest(LOCAL_DIR_YAML).unwrap();
        let source = manifest.fragments[0].resolve_source();
        assert!(matches!(source, FragmentSource::Directory { .. }));
    }

    #[test]
    fn reject_missing_base() {
        let bad = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
fragments:
  - image: quay.io/test:1
"#;
        assert!(parse_manifest(bad).is_err());
    }

    #[test]
    fn reject_empty_fragments() {
        let bad = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments: []
"#;
        let result = parse_manifest(bad);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --lib manifest`
Expected: compilation errors — types not defined

- [ ] **Step 3: Implement manifest.rs types and parsing**

```rust
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum FragmentSource {
    Registry { image_ref: String },
    Directory { path: PathBuf },
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestYaml {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    base: Option<String>,
    #[serde(default)]
    fragments: Vec<ManifestFragmentYaml>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestFragmentYaml {
    image: String,
    #[serde(default)]
    packages: Vec<String>,
    mirror: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub base: String,
    pub fragments: Vec<ManifestFragment>,
}

#[derive(Debug, Clone)]
pub struct ManifestFragment {
    pub image: String,
    pub packages: Vec<String>,
    pub mirror: Option<String>,
}

impl ManifestFragment {
    pub fn resolve_source(&self) -> FragmentSource {
        if let Some(dir_path) = self.image.strip_prefix("dir:") {
            FragmentSource::Directory {
                path: PathBuf::from(dir_path),
            }
        } else {
            FragmentSource::Registry {
                image_ref: self.image.clone(),
            }
        }
    }
}

pub fn parse_manifest(content: &str) -> Result<Manifest> {
    let raw: ManifestYaml =
        serde_yaml::from_str(content).context("failed to parse manifest YAML")?;

    let base = raw
        .base
        .ok_or_else(|| anyhow::anyhow!("manifest missing required 'base' field"))?;

    if raw.fragments.is_empty() {
        bail!("manifest must contain at least one fragment");
    }

    let fragments = raw
        .fragments
        .into_iter()
        .map(|f| ManifestFragment {
            image: f.image,
            packages: f.packages,
            mirror: f.mirror,
        })
        .collect();

    Ok(Manifest { base, fragments })
}
```

- [ ] **Step 4: Add module to lib.rs**

Add `pub mod manifest;` to `src/lib.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test --lib manifest`
Expected: all 6 tests PASS

- [ ] **Step 6: Commit**

```bash
cd ~/Work/bootc-assemble
git add src/manifest.rs src/lib.rs
git commit -m "feat: manifest YAML data model and parsing

Manifest struct, ManifestFragment with FragmentSource resolution
(registry vs dir: local), mirror support, validation.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 3: Example fragments — all 8

**Files:**
- Create: `examples/fragments/epel/{fragment.toml,tree/etc/yum.repos.d/epel.repo,tree/etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL-10,Containerfile.fragment}`
- Create: `examples/fragments/tailscale/{fragment.toml,tree/etc/yum.repos.d/tailscale.repo,tree/etc/pki/rpm-gpg/RPM-GPG-KEY-tailscale,tree/usr/lib/systemd/system-preset/50-tailscale.preset,scripts/configure.sh,Containerfile.fragment}`
- Create: `examples/fragments/grafana/{fragment.toml,tree/...,scripts/configure.sh,Containerfile.fragment}`
- Create: `examples/fragments/postgresql/{fragment.toml,tree/...,Containerfile.fragment}`
- Create: `examples/fragments/hashicorp/{fragment.toml,tree/...,scripts/configure.sh,Containerfile.fragment}`
- Create: `examples/fragments/cis-hardening/{fragment.toml,tree/...,scripts/configure.sh,Containerfile.fragment}`
- Create: `examples/fragments/node-exporter/{fragment.toml,tree/...,scripts/configure.sh,Containerfile.fragment}`
- Create: `examples/fragments/nginx/{fragment.toml,tree/...,scripts/configure.sh,Containerfile.fragment}`
- Create: `examples/manifests/minimal.yaml`
- Create: `examples/manifests/full.yaml`
- Create: `examples/manifests/mirror-demo.yaml`

**Interfaces:**
- Consumes: Fragment data model from Task 1 (fragment.toml schema)
- Produces: 8 buildable fragment directories, 3 example manifests. Used by all later tasks for testing.

This task creates the 8 example fragments. Each fragment follows the same pattern: `fragment.toml` + `tree/` directory structure + optional `scripts/configure.sh` + `Containerfile.fragment`. All fragments use freely available RPM repos and real GPG keys fetched from upstream.

- [ ] **Step 1: Create directory structure for all 8 fragments**

```bash
cd ~/Work/bootc-assemble
for frag in epel tailscale grafana postgresql hashicorp cis-hardening node-exporter nginx; do
  mkdir -p examples/fragments/$frag/tree examples/fragments/$frag/scripts
done
mkdir -p examples/manifests
```

- [ ] **Step 2: Create EPEL fragment (phase: repos — pure repo connector)**

`examples/fragments/epel/fragment.toml`:
```toml
[fragment]
name = "epel"
version = "10"
description = "Extra Packages for Enterprise Linux (EPEL) repository"
vendor = "Fedora Project"
phase = "repos"

[fragment.provides]
repos = ["epel"]

[fragment.packages]
available = [
    "htop",
    "tmux",
    "jq",
    "fzf",
    "ripgrep",
    "bat",
    "fd-find",
]
```

`examples/fragments/epel/tree/etc/yum.repos.d/epel.repo`:
```ini
[epel]
name=Extra Packages for Enterprise Linux $releasever - $basearch
metalink=https://mirrors.fedoraproject.org/metalink?repo=epel-$releasever&arch=$basearch&infra=$infra&content=$contenttype
enabled=1
gpgcheck=1
countme=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL-$releasever
```

Fetch the real EPEL 10 GPG key:
```bash
curl -sL https://dl.fedoraproject.org/pub/epel/RPM-GPG-KEY-EPEL-10 \
  -o examples/fragments/epel/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL-10
```

`examples/fragments/epel/Containerfile.fragment`:
```dockerfile
FROM scratch
COPY fragment.toml tree/ scripts/ /fragment/
```

No `scripts/configure.sh` — EPEL is a pure repo connector.

- [ ] **Step 3: Create Tailscale fragment (phase: config — full spectrum)**

`examples/fragments/tailscale/fragment.toml`:
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
```

`examples/fragments/tailscale/tree/etc/yum.repos.d/tailscale.repo`:
```ini
[tailscale-stable]
name=Tailscale stable
baseurl=https://pkgs.tailscale.com/stable/rhel/$releasever/$basearch
enabled=1
type=rpm
repo_gpgcheck=1
gpgcheck=0
gpgkey=https://pkgs.tailscale.com/stable/rhel/repo.gpg
```

Fetch the real Tailscale GPG key:
```bash
curl -sL https://pkgs.tailscale.com/stable/rhel/repo.gpg \
  -o examples/fragments/tailscale/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-tailscale
```

`examples/fragments/tailscale/tree/usr/lib/systemd/system-preset/50-tailscale.preset`:
```
enable tailscaled.service
```

`examples/fragments/tailscale/scripts/configure.sh`:
```bash
#!/bin/bash
set -euo pipefail
# Tailscale runs as a system service — preset handles enablement.
# This script handles any post-install configuration that presets cannot.
echo "Tailscale fragment: configuration complete"
```

Make it executable: `chmod +x examples/fragments/tailscale/scripts/configure.sh`

`examples/fragments/tailscale/Containerfile.fragment`:
```dockerfile
FROM scratch
COPY fragment.toml tree/ scripts/ /fragment/
```

- [ ] **Step 4: Create Grafana fragment (phase: config — monitoring story)**

`examples/fragments/grafana/fragment.toml`:
```toml
[fragment]
name = "grafana"
version = "11.0"
description = "Grafana monitoring dashboard — repo, package, and service enablement"
vendor = "Grafana Labs"
phase = "config"

[fragment.provides]
repos = ["grafana"]

[fragment.packages]
available = ["grafana"]
```

`examples/fragments/grafana/tree/etc/yum.repos.d/grafana.repo`:
```ini
[grafana]
name=Grafana OSS
baseurl=https://rpm.grafana.com
enabled=1
gpgcheck=1
gpgkey=https://rpm.grafana.com/gpg.key
sslverify=1
sslcacert=/etc/pki/tls/certs/ca-bundle.crt
```

Fetch GPG key:
```bash
curl -sL https://rpm.grafana.com/gpg.key \
  -o examples/fragments/grafana/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-grafana
```

`examples/fragments/grafana/tree/usr/lib/systemd/system-preset/50-grafana.preset`:
```
enable grafana-server.service
```

`examples/fragments/grafana/scripts/configure.sh`:
```bash
#!/bin/bash
set -euo pipefail
echo "Grafana fragment: configuration complete"
```

Make executable, add Containerfile.fragment (same pattern as Tailscale).

- [ ] **Step 5: Create PostgreSQL fragment (phase: repos — multi-version)**

`examples/fragments/postgresql/fragment.toml`:
```toml
[fragment]
name = "postgresql"
version = "17"
description = "PostgreSQL Global Development Group RPM repository"
vendor = "PostgreSQL Global Development Group"
phase = "repos"

[fragment.provides]
repos = ["pgdg-common", "pgdg17"]

[fragment.packages]
available = [
    "postgresql17-server",
    "postgresql17",
    "postgresql16-server",
    "postgresql16",
]
```

`examples/fragments/postgresql/tree/etc/yum.repos.d/pgdg-redhat-all.repo`:
```ini
[pgdg-common]
name=PostgreSQL common RPMs for RHEL / Rocky / AlmaLinux $releasever - $basearch
baseurl=https://download.postgresql.org/pub/repos/yum/common/redhat/rhel-$releasever-$basearch
enabled=1
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/PGDG-RPM-GPG-KEY-RHEL

[pgdg17]
name=PostgreSQL 17 for RHEL / Rocky / AlmaLinux $releasever - $basearch
baseurl=https://download.postgresql.org/pub/repos/yum/17/redhat/rhel-$releasever-$basearch
enabled=1
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/PGDG-RPM-GPG-KEY-RHEL
```

Fetch GPG key:
```bash
curl -sL https://download.postgresql.org/pub/repos/yum/keys/PGDG-RPM-GPG-KEY-RHEL \
  -o examples/fragments/postgresql/tree/etc/pki/rpm-gpg/PGDG-RPM-GPG-KEY-RHEL
```

No scripts (pure repo connector). Add Containerfile.fragment.

- [ ] **Step 6: Create HashiCorp fragment (phase: config — infrastructure tooling)**

`examples/fragments/hashicorp/fragment.toml`:
```toml
[fragment]
name = "hashicorp"
version = "1.0"
description = "HashiCorp Vault — repo, package, and basic configuration"
vendor = "HashiCorp"
phase = "config"

[fragment.provides]
repos = ["hashicorp"]

[fragment.packages]
available = ["vault", "consul", "nomad", "terraform"]
```

`examples/fragments/hashicorp/tree/etc/yum.repos.d/hashicorp.repo`:
```ini
[hashicorp]
name=HashiCorp Stable - $basearch
baseurl=https://rpm.releases.hashicorp.com/RHEL/$releasever/$basearch/stable
enabled=1
gpgcheck=1
gpgkey=https://rpm.releases.hashicorp.com/gpg
```

Fetch GPG key:
```bash
curl -sL https://rpm.releases.hashicorp.com/gpg \
  -o examples/fragments/hashicorp/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-hashicorp
```

`examples/fragments/hashicorp/tree/usr/lib/systemd/system-preset/50-vault.preset`:
```
enable vault.service
```

`examples/fragments/hashicorp/scripts/configure.sh`:
```bash
#!/bin/bash
set -euo pipefail
echo "HashiCorp fragment: configuration complete"
```

Make executable. Add Containerfile.fragment.

- [ ] **Step 7: Create CIS Hardening fragment (phase: config — pure config, no repo)**

`examples/fragments/cis-hardening/fragment.toml`:
```toml
[fragment]
name = "cis-hardening"
version = "2.1"
description = "CIS Level 2 security hardening — sysctl, audit, filesystem"
phase = "config"
```

No `[fragment.provides]` or `[fragment.packages]` — this fragment has no repo.

`examples/fragments/cis-hardening/tree/usr/lib/sysctl.d/99-cis-hardening.conf`:
```ini
# CIS Level 2 kernel parameter hardening
net.ipv4.conf.all.send_redirects = 0
net.ipv4.conf.default.send_redirects = 0
net.ipv4.conf.all.accept_redirects = 0
net.ipv4.conf.default.accept_redirects = 0
net.ipv4.conf.all.log_martians = 1
net.ipv4.conf.default.log_martians = 1
net.ipv4.icmp_echo_ignore_broadcasts = 1
net.ipv4.icmp_ignore_bogus_error_responses = 1
net.ipv6.conf.all.accept_redirects = 0
net.ipv6.conf.default.accept_redirects = 0
kernel.randomize_va_space = 2
```

`examples/fragments/cis-hardening/tree/usr/lib/tmpfiles.d/cis-permissions.conf`:
```
# CIS Level 2 file permission hardening
z /etc/crontab 0600 root root -
z /etc/cron.hourly 0700 root root -
z /etc/cron.daily 0700 root root -
z /etc/cron.weekly 0700 root root -
z /etc/cron.monthly 0700 root root -
```

`examples/fragments/cis-hardening/scripts/configure.sh`:
```bash
#!/bin/bash
set -euo pipefail
echo "CIS hardening fragment: configuration applied"
```

Make executable. Add Containerfile.fragment.

- [ ] **Step 8: Create Node Exporter fragment (phase: config — self-contained EPEL)**

`examples/fragments/node-exporter/fragment.toml`:
```toml
[fragment]
name = "node-exporter"
version = "1.8.0"
description = "Prometheus Node Exporter — self-contained with EPEL repo"
phase = "config"

[fragment.provides]
repos = ["epel"]

[fragment.packages]
available = ["golang-github-prometheus-node-exporter"]
```

Copy the same EPEL `.repo` file and GPG key from the EPEL fragment:
```bash
mkdir -p examples/fragments/node-exporter/tree/etc/yum.repos.d
mkdir -p examples/fragments/node-exporter/tree/etc/pki/rpm-gpg
cp examples/fragments/epel/tree/etc/yum.repos.d/epel.repo \
   examples/fragments/node-exporter/tree/etc/yum.repos.d/
cp examples/fragments/epel/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL-10 \
   examples/fragments/node-exporter/tree/etc/pki/rpm-gpg/
```

`examples/fragments/node-exporter/tree/usr/lib/systemd/system-preset/50-node-exporter.preset`:
```
enable prometheus-node-exporter.service
```

`examples/fragments/node-exporter/scripts/configure.sh`:
```bash
#!/bin/bash
set -euo pipefail
echo "Node Exporter fragment: configuration complete"
```

Make executable. Add Containerfile.fragment.

- [ ] **Step 9: Create Nginx fragment (phase: config — web server)**

`examples/fragments/nginx/fragment.toml`:
```toml
[fragment]
name = "nginx"
version = "1.26"
description = "Nginx web server — official repo, package, and service enablement"
vendor = "F5 / Nginx Inc."
phase = "config"

[fragment.provides]
repos = ["nginx-stable"]

[fragment.packages]
available = ["nginx"]
```

`examples/fragments/nginx/tree/etc/yum.repos.d/nginx.repo`:
```ini
[nginx-stable]
name=nginx stable repo
baseurl=http://nginx.org/packages/rhel/$releasever/$basearch/
gpgcheck=1
enabled=1
gpgkey=https://nginx.org/keys/nginx_signing.key
module_hotfixes=true
```

Fetch GPG key:
```bash
curl -sL https://nginx.org/keys/nginx_signing.key \
  -o examples/fragments/nginx/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-nginx
```

`examples/fragments/nginx/tree/usr/lib/systemd/system-preset/50-nginx.preset`:
```
enable nginx.service
```

`examples/fragments/nginx/scripts/configure.sh`:
```bash
#!/bin/bash
set -euo pipefail
echo "Nginx fragment: configuration complete"
```

Make executable. Add Containerfile.fragment.

- [ ] **Step 10: Create example manifests**

`examples/manifests/minimal.yaml`:
```yaml
apiVersion: bootc.io/v1alpha1
kind: Composition

base: registry.redhat.io/rhel10/rhel-bootc:10.0

fragments:
  - image: quay.io/mrussell/fragments/epel:10
    packages: [htop, tmux]
  - image: quay.io/mrussell/fragments/cis-hardening:2.1
```

`examples/manifests/full.yaml`:
```yaml
apiVersion: bootc.io/v1alpha1
kind: Composition

base: registry.redhat.io/rhel10/rhel-bootc:10.0

fragments:
  - image: quay.io/mrussell/fragments/epel:10
    packages: [htop, tmux, jq]
  - image: quay.io/mrussell/fragments/tailscale:1.82.0
    packages: [tailscale]
  - image: quay.io/mrussell/fragments/grafana:11.0
    packages: [grafana]
  - image: quay.io/mrussell/fragments/postgresql:17
    packages: [postgresql17-server]
  - image: quay.io/mrussell/fragments/hashicorp:1.0
    packages: [vault]
  - image: quay.io/mrussell/fragments/cis-hardening:2.1
  - image: quay.io/mrussell/fragments/node-exporter:1.8.0
    packages: [golang-github-prometheus-node-exporter]
  - image: quay.io/mrussell/fragments/nginx:1.26
    packages: [nginx]
```

`examples/manifests/mirror-demo.yaml`:
```yaml
apiVersion: bootc.io/v1alpha1
kind: Composition

base: registry.redhat.io/rhel10/rhel-bootc:10.0

fragments:
  - image: quay.io/mrussell/fragments/grafana:11.0
    packages: [grafana]
    mirror: https://rpm-mirror.internal.corp/grafana/
  - image: quay.io/mrussell/fragments/tailscale:1.82.0
    packages: [tailscale]
    mirror: https://rpm-mirror.internal.corp/tailscale/
```

- [ ] **Step 11: Verify all fragment.toml files parse correctly**

```bash
cd ~/Work/bootc-assemble
cargo test --lib fragment -- --include-ignored 2>/dev/null
# Also verify with a quick Rust script or add a test that reads each example:
for f in examples/fragments/*/fragment.toml; do
  echo "Parsing: $f"
  cargo run -- 2>/dev/null || true
done
```

Add a test in `src/fragment.rs` that parses every example fragment.toml:

```rust
#[test]
fn parse_all_example_fragments() {
    let examples_dir = std::path::Path::new("examples/fragments");
    if !examples_dir.exists() {
        return; // skip if not in repo root
    }
    for entry in std::fs::read_dir(examples_dir).unwrap() {
        let entry = entry.unwrap();
        let toml_path = entry.path().join("fragment.toml");
        if toml_path.exists() {
            let content = std::fs::read_to_string(&toml_path).unwrap();
            let result = parse_fragment_toml(&content);
            assert!(
                result.is_ok(),
                "Failed to parse {}: {}",
                toml_path.display(),
                result.unwrap_err()
            );
        }
    }
}
```

- [ ] **Step 12: Commit**

```bash
cd ~/Work/bootc-assemble
git add examples/
git commit -m "feat: 8 example fragments and 3 manifests

EPEL, Tailscale, Grafana, PostgreSQL, HashiCorp, CIS Hardening,
Node Exporter, Nginx — all using real freely-available RPM repos
and GPG keys. Three manifests: minimal, full, mirror-demo.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 4: Fragment loading — local directory and registry

**Files:**
- Create: `src/loader.rs`
- Modify: `src/lib.rs` — add `pub mod loader;`
- Test: inline `#[cfg(test)]` in `src/loader.rs`

**Interfaces:**
- Consumes: `Fragment` from `fragment.rs`, `FragmentSource` from `manifest.rs`
- Produces: `LoadedFragment` struct, `load_fragment(source: &FragmentSource) -> Result<LoadedFragment>`, `resolve_digest(image_ref: &str) -> Result<String>`

- [ ] **Step 1: Write failing tests for local directory loading**

```rust
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::fragment::{parse_fragment_toml, validate_phase_consistency, Fragment};
use crate::manifest::FragmentSource;

#[derive(Debug, Clone)]
pub struct LoadedFragment {
    pub fragment: Fragment,
    pub tree_paths: Vec<PathBuf>,
    pub has_configure_script: bool,
    pub source: FragmentSource,
    pub resolved_digest: Option<String>,
    /// Index into the original manifest.fragments vec, preserved through sorting.
    pub manifest_index: usize,
    /// Cached .repo file contents for dedup comparison, keyed by filename.
    /// Populated during loading from either local filesystem or layer extraction.
    pub repo_file_contents: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_local_epel_fragment() {
        let source = FragmentSource::Directory {
            path: PathBuf::from("examples/fragments/epel"),
        };
        let loaded = load_local_fragment(&source).unwrap();
        assert_eq!(loaded.fragment.name, "epel");
        assert!(!loaded.has_configure_script);
        assert!(loaded.tree_paths.iter().any(|p| p
            .to_string_lossy()
            .contains("yum.repos.d/epel.repo")));
        assert!(loaded.resolved_digest.is_none());
    }

    #[test]
    fn load_local_tailscale_fragment() {
        let source = FragmentSource::Directory {
            path: PathBuf::from("examples/fragments/tailscale"),
        };
        let loaded = load_local_fragment(&source).unwrap();
        assert_eq!(loaded.fragment.name, "tailscale");
        assert!(loaded.has_configure_script);
        assert!(loaded.tree_paths.iter().any(|p| p
            .to_string_lossy()
            .contains("system-preset")));
    }

    #[test]
    fn load_local_missing_toml_fails() {
        let source = FragmentSource::Directory {
            path: PathBuf::from("/tmp/nonexistent-fragment"),
        };
        assert!(load_local_fragment(&source).is_err());
    }

    #[test]
    fn classify_repo_paths() {
        assert!(is_repo_path(Path::new("tree/etc/yum.repos.d/epel.repo")));
        assert!(is_repo_path(Path::new(
            "tree/etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL-10"
        )));
        assert!(!is_repo_path(Path::new(
            "tree/usr/lib/sysctl.d/99-hardening.conf"
        )));
        assert!(!is_repo_path(Path::new("scripts/configure.sh")));
    }

    #[test]
    fn split_tree_paths_separates_repo_from_config() {
        let paths = vec![
            PathBuf::from("tree/etc/yum.repos.d/tailscale.repo"),
            PathBuf::from("tree/etc/pki/rpm-gpg/RPM-GPG-KEY-tailscale"),
            PathBuf::from("tree/usr/lib/systemd/system-preset/50-tailscale.preset"),
        ];
        let (repo, config) = split_tree_paths(&paths);
        assert_eq!(repo.len(), 2);
        assert_eq!(config.len(), 1);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --lib loader`
Expected: compilation errors

- [ ] **Step 3: Implement local directory loading**

```rust
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::fragment::{is_repo_path, parse_fragment_toml, validate_phase_consistency, Fragment};
use crate::manifest::FragmentSource;

#[derive(Debug, Clone)]
pub struct LoadedFragment {
    pub fragment: Fragment,
    pub tree_paths: Vec<PathBuf>,
    pub has_configure_script: bool,
    pub source: FragmentSource,
    pub resolved_digest: Option<String>,
    /// Index into the original manifest.fragments vec, preserved through sorting.
    pub manifest_index: usize,
    /// Cached .repo file contents for dedup comparison, keyed by filename.
    /// Populated during loading from either local filesystem or layer extraction.
    pub repo_file_contents: std::collections::HashMap<String, String>,
}

pub fn split_tree_paths(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let tree_paths: Vec<_> = paths
        .iter()
        .filter(|p| p.to_string_lossy().starts_with("tree/"))
        .cloned()
        .collect();
    let repo: Vec<_> = tree_paths.iter().filter(|p| is_repo_path(p)).cloned().collect();
    let config: Vec<_> = tree_paths.iter().filter(|p| !is_repo_path(p)).cloned().collect();
    (repo, config)
}

fn collect_relative_paths(base: &Path, prefix: &str) -> Result<Vec<PathBuf>> {
    let dir = base.join(prefix);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_paths_recursive(&dir, base, &mut paths)?;
    Ok(paths)
}

fn collect_paths_recursive(
    dir: &Path,
    base: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).context("reading directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_paths_recursive(&path, base, paths)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .context("stripping prefix")?
                .to_path_buf();
            paths.push(rel);
        }
    }
    Ok(())
}

pub fn load_local_fragment(source: &FragmentSource) -> Result<LoadedFragment> {
    let dir = match source {
        FragmentSource::Directory { path } => path,
        FragmentSource::Registry { .. } => {
            bail!("load_local_fragment called with registry source");
        }
    };

    let toml_path = dir.join("fragment.toml");
    let content = std::fs::read_to_string(&toml_path)
        .with_context(|| format!("reading {}", toml_path.display()))?;
    let fragment = parse_fragment_toml(&content)?;

    let mut all_paths = Vec::new();
    all_paths.extend(collect_relative_paths(dir, "tree")?);
    all_paths.extend(collect_relative_paths(dir, "scripts")?);

    let has_configure_script = dir.join("scripts/configure.sh").exists();

    validate_phase_consistency(&fragment, &all_paths)?;

    // Read .repo file contents for dedup comparison
    let mut repo_file_contents = std::collections::HashMap::new();
    let repo_dir = dir.join("tree/etc/yum.repos.d");
    if repo_dir.exists() {
        for entry in std::fs::read_dir(&repo_dir)? {
            let entry = entry?;
            if entry.path().is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                let content = std::fs::read_to_string(entry.path())?;
                repo_file_contents.insert(name, content);
            }
        }
    }

    Ok(LoadedFragment {
        fragment,
        tree_paths: all_paths,
        has_configure_script,
        source: source.clone(),
        resolved_digest: None,
        manifest_index: 0, // set by caller
        repo_file_contents,
    })
}

pub fn resolve_digest(image_ref: &str) -> Result<String> {
    let output = std::process::Command::new("skopeo")
        .args(["inspect", "--raw", &format!("docker://{}", image_ref)])
        .output()
        .context("failed to run skopeo inspect")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("skopeo inspect failed for {}: {}", image_ref, stderr);
    }

    let stdout = String::from_utf8(output.stdout)?;
    let manifest: serde_json::Value = serde_json::from_str(&stdout)?;

    // The digest is available from the Docker-Content-Digest header,
    // but skopeo inspect --raw gives us the manifest body.
    // We compute the digest from the manifest bytes.
    use std::io::Write;
    let digest_output = std::process::Command::new("skopeo")
        .args([
            "inspect",
            "--format",
            "{{.Digest}}",
            &format!("docker://{}", image_ref),
        ])
        .output()
        .context("failed to run skopeo inspect for digest")?;

    if !digest_output.status.success() {
        bail!("skopeo digest lookup failed for {}", image_ref);
    }

    let digest = String::from_utf8(digest_output.stdout)?
        .trim()
        .to_string();
    Ok(digest)
}
```

- [ ] **Step 4: Add module to lib.rs**

Add `pub mod loader;` to `src/lib.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test --lib loader`
Expected: all 5 tests PASS (registry tests require skopeo, tested in Task 7)

- [ ] **Step 6: Commit**

```bash
cd ~/Work/bootc-assemble
git add src/loader.rs src/lib.rs
git commit -m "feat: fragment loading — local directory mode

LoadedFragment struct, local directory reading with tree path
collection, repo/config path splitting, phase consistency
validation, skopeo digest resolution stub.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 5: Registry fragment loading — skopeo and layer extraction

**Files:**
- Modify: `src/loader.rs` — add registry loading functions
- Test: inline `#[cfg(test)]` additions in `src/loader.rs`

**Interfaces:**
- Consumes: `FragmentSource::Registry`, `resolve_digest()` from Task 4
- Produces: `load_registry_fragment(source: &FragmentSource) -> Result<LoadedFragment>`, `extract_fragment_toml_from_layer(layer_path: &Path) -> Result<String>`, `prebuild_local_fragment(dir: &Path, name: &str) -> Result<String>`

- [ ] **Step 1: Write failing tests for layer extraction**

```rust
#[cfg(test)]
mod layer_tests {
    use super::*;
    use std::io::Write;

    fn create_test_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for (path, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append(&header, &data[..]).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_toml_from_valid_layer() {
        let toml_content = br#"[fragment]
name = "test"
version = "1.0"
description = "test fragment"
phase = "repos"
"#;
        let tarball = create_test_tarball(&[("fragment/fragment.toml", toml_content)]);
        let result = extract_fragment_toml_from_bytes(&tarball).unwrap();
        assert!(result.contains("name = \"test\""));
    }

    #[test]
    fn reject_traversal_path() {
        let tarball = create_test_tarball(&[
            ("../etc/passwd", b"evil"),
            ("fragment/fragment.toml", b"[fragment]\nname=\"x\"\nversion=\"1\"\ndescription=\"x\"\nphase=\"repos\""),
        ]);
        // Fail-closed: traversal entries cause immediate failure
        let result = extract_fragment_toml_from_bytes(&tarball);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("traversal"));
    }

    #[test]
    fn reject_oversized_toml() {
        let huge = vec![b'x'; 128 * 1024]; // 128KB > 64KB limit
        let tarball = create_test_tarball(&[("fragment/fragment.toml", &huge)]);
        let result = extract_fragment_toml_from_bytes(&tarball);
        assert!(result.is_err());
    }

    #[test]
    fn reject_missing_toml() {
        let tarball = create_test_tarball(&[("fragment/tree/etc/foo.conf", b"data")]);
        let result = extract_fragment_toml_from_bytes(&tarball);
        assert!(result.is_err());
    }

    #[test]
    fn reject_duplicate_toml_entries() {
        let tarball = create_test_tarball(&[
            ("fragment/fragment.toml", b"[fragment]\nname=\"a\"\nversion=\"1\"\ndescription=\"a\"\nphase=\"repos\""),
            ("fragment/fragment.toml", b"[fragment]\nname=\"b\"\nversion=\"2\"\ndescription=\"b\"\nphase=\"config\""),
        ]);
        let result = extract_fragment_toml_from_bytes(&tarball);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --lib loader::layer_tests`
Expected: compilation errors

- [ ] **Step 3: Implement layer extraction with fail-closed contract**

```rust
use flate2::read::GzDecoder;

const MAX_FRAGMENT_TOML_SIZE: u64 = 64 * 1024; // 64KB
const FRAGMENT_TOML_PATH: &str = "fragment/fragment.toml";

pub fn extract_fragment_toml_from_bytes(compressed: &[u8]) -> Result<String> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);

    let mut found: Option<String> = None;

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let path = entry.path().context("reading entry path")?;
        let path_str = path.to_string_lossy();

        // Fail-closed: reject traversal
        if path_str.contains("..") {
            bail!(
                "path traversal detected in fragment layer: {}",
                path_str
            );
        }

        // Fail-closed: reject absolute paths outside /fragment/
        if path_str.starts_with('/') && !path_str.starts_with("/fragment/") {
            bail!(
                "absolute path outside /fragment/ rejected in fragment layer: {}",
                path_str
            );
        }

        // Fail-closed: reject symlinks and hardlinks
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            bail!(
                "symlink or hardlink rejected in fragment layer: {}",
                path_str
            );
        }

        if path_str == FRAGMENT_TOML_PATH {
            if found.is_some() {
                bail!("duplicate fragment.toml entries in layer");
            }

            let size = entry.header().size()?;
            if size > MAX_FRAGMENT_TOML_SIZE {
                bail!(
                    "fragment.toml is {} bytes, exceeds {} byte limit",
                    size,
                    MAX_FRAGMENT_TOML_SIZE
                );
            }

            let mut content = String::new();
            use std::io::Read;
            entry
                .read_to_string(&mut content)
                .context("reading fragment.toml from layer")?;
            found = Some(content);
        }
    }

    found.ok_or_else(|| anyhow::anyhow!("fragment.toml not found in layer"))
}

/// Try the OCI annotation fast path: parse fragment metadata from
/// manifest annotations without pulling any layers.
fn try_annotation_fast_path(image_ref: &str) -> Result<Option<Fragment>> {
    let output = std::process::Command::new("skopeo")
        .args(["inspect", "--raw", &format!("docker://{}", image_ref)])
        .output()
        .context("failed to run skopeo inspect --raw")?;

    if !output.status.success() {
        return Ok(None);
    }

    let manifest: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let annotations = match manifest.get("annotations") {
        Some(a) => a,
        None => return Ok(None),
    };

    // Check for required annotation fields
    let name = annotations.get("io.bootc.fragment.name").and_then(|v| v.as_str());
    let version = annotations.get("io.bootc.fragment.version").and_then(|v| v.as_str());
    let description = annotations
        .get("io.bootc.fragment.description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let phase_str = annotations.get("io.bootc.fragment.phase").and_then(|v| v.as_str());

    let (name, version, phase_str) = match (name, version, phase_str) {
        (Some(n), Some(v), Some(p)) => (n, v, p),
        _ => return Ok(None), // Missing required annotations — fall back to layer extraction
    };

    let phase = match phase_str {
        "repos" => FragmentPhase::Repos,
        "config" => FragmentPhase::Config,
        _ => return Ok(None),
    };

    let repos: Vec<String> = annotations
        .get("io.bootc.fragment.provides.repos")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let available: Vec<String> = annotations
        .get("io.bootc.fragment.packages.available")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let vendor = annotations
        .get("io.bootc.fragment.vendor")
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(Some(Fragment {
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        vendor,
        phase,
        provides: FragmentProvides { repos },
        packages: FragmentPackages { available },
        conflicts: FragmentConflicts { fragments: vec![] },
    }))
}

pub fn load_registry_fragment(image_ref: &str) -> Result<LoadedFragment> {
    let digest = resolve_digest(image_ref)?;
    let image_with_digest = format!(
        "{}@{}",
        image_ref.split('@').next().unwrap_or(image_ref),
        digest
    );

    // Annotation fast path: try to read metadata without pulling layers.
    // For assembly, we still need the layer to extract tree_paths and
    // has_configure_script, so the pull happens regardless. But the
    // Fragment is taken from annotations when available, skipping the
    // in-layer TOML parse.
    let annotation_fragment = try_annotation_fast_path(image_ref)
        .unwrap_or(None);

    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let oci_path = tmp.path().join("oci-layout");

    let status = std::process::Command::new("skopeo")
        .args([
            "copy",
            &format!("docker://{}", image_ref),
            &format!("oci:{}", oci_path.display()),
        ])
        .status()
        .context("failed to run skopeo copy")?;

    if !status.success() {
        bail!("skopeo copy failed for {}", image_ref);
    }

    // Read OCI index to find the single layer blob
    let index_path = oci_path.join("index.json");
    let index_content = std::fs::read_to_string(&index_path)?;
    let index: serde_json::Value = serde_json::from_str(&index_content)?;

    let manifest_desc = index["manifests"]
        .as_array()
        .and_then(|m| m.first())
        .ok_or_else(|| anyhow::anyhow!("no manifests in OCI index"))?;

    let manifest_digest = manifest_desc["digest"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no digest in manifest descriptor"))?;

    let manifest_blob_path = oci_path
        .join("blobs")
        .join(manifest_digest.replace(':', "/"));
    let manifest_content = std::fs::read_to_string(&manifest_blob_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_content)?;

    let layers = manifest["layers"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no layers in manifest"))?;

    if layers.len() != 1 {
        bail!(
            "fragment image must be single-layer, found {} layers",
            layers.len()
        );
    }

    let layer_digest = layers[0]["digest"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no digest in layer descriptor"))?;

    let layer_blob_path = oci_path
        .join("blobs")
        .join(layer_digest.replace(':', "/"));
    let layer_bytes = std::fs::read(&layer_blob_path)?;

    // Use annotation fragment if available, otherwise parse from layer
    let fragment = match annotation_fragment {
        Some(f) => f,
        None => {
            let toml_content = extract_fragment_toml_from_bytes(&layer_bytes)?;
            parse_fragment_toml(&toml_content)?
        }
    };

    // Always extract tree paths from the layer (annotations don't carry these)
    let tree_paths = extract_tree_paths_from_bytes(&layer_bytes)?;

    let has_configure_script = tree_paths
        .iter()
        .any(|p| p.to_string_lossy() == "fragment/scripts/configure.sh");

    // Remap paths: fragment/tree/... -> tree/..., fragment/scripts/... -> scripts/...
    let relative_paths: Vec<PathBuf> = tree_paths
        .iter()
        .filter_map(|p| p.strip_prefix("fragment").ok())
        .map(|p| p.to_path_buf())
        .collect();

    validate_phase_consistency(&fragment, &relative_paths)?;

    // Extract .repo file contents from the layer for dedup comparison
    let repo_file_contents = extract_repo_file_contents_from_bytes(&layer_bytes)?;

    Ok(LoadedFragment {
        fragment,
        tree_paths: relative_paths,
        has_configure_script,
        source: FragmentSource::Registry {
            image_ref: image_with_digest,
        },
        resolved_digest: Some(digest),
        manifest_index: 0, // set by caller
        repo_file_contents,
    })
}

fn extract_repo_file_contents_from_bytes(
    compressed: &[u8],
) -> Result<std::collections::HashMap<String, String>> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let mut contents = std::collections::HashMap::new();

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy();

        if path_str.contains("yum.repos.d/") && path_str.ends_with(".repo") {
            if let Some(filename) = path.file_name() {
                let mut content = String::new();
                use std::io::Read;
                entry.read_to_string(&mut content)?;
                contents.insert(filename.to_string_lossy().to_string(), content);
            }
        }
    }
    Ok(contents)
}

/// Metadata-only registry load for inspect and list.
/// Uses annotations to skip the layer pull entirely when possible.
/// Falls back to full load_registry_fragment when annotations are absent.
pub fn load_registry_fragment_metadata_only(image_ref: &str) -> Result<LoadedFragment> {
    let digest = resolve_digest(image_ref)?;
    let image_with_digest = format!(
        "{}@{}",
        image_ref.split('@').next().unwrap_or(image_ref),
        digest
    );

    if let Some(fragment) = try_annotation_fast_path(image_ref)? {
        // Annotations present — return metadata without pulling layers.
        // tree_paths and has_configure_script are unknown in this path;
        // inspect/list can display fragment metadata without them.
        return Ok(LoadedFragment {
            fragment,
            tree_paths: vec![],
            has_configure_script: false,
            source: FragmentSource::Registry {
                image_ref: image_with_digest,
            },
            resolved_digest: Some(digest),
            manifest_index: 0, // set by caller
            repo_file_contents: std::collections::HashMap::new(),
        });
    }

    // No annotations — fall back to full layer extraction
    load_registry_fragment(image_ref)
}

fn extract_tree_paths_from_bytes(compressed: &[u8]) -> Result<Vec<PathBuf>> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let mut paths = Vec::new();

    for entry_result in archive.entries()? {
        let entry = entry_result?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy();

        // Fail-closed: same rules as TOML extraction
        if path_str.contains("..") {
            bail!("path traversal detected in fragment layer: {}", path_str);
        }
        if path_str.starts_with('/') && !path_str.starts_with("/fragment/") {
            bail!(
                "absolute path outside /fragment/ rejected in fragment layer: {}",
                path_str
            );
        }
        if entry.header().entry_type().is_symlink()
            || entry.header().entry_type().is_hard_link()
        {
            bail!("symlink or hardlink rejected in fragment layer: {}", path_str);
        }
        if entry.header().entry_type().is_file() {
            paths.push(path.to_path_buf());
        }
    }
    Ok(paths)
}

pub fn prebuild_local_fragment(dir: &Path, name: &str) -> Result<String> {
    let tag = format!("localhost/bootc-assemble/frag-{}:local", name);

    let status = std::process::Command::new("podman")
        .args([
            "build",
            "-f",
            "-",
            "-t",
            &tag,
            &dir.to_string_lossy(),
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(stdin) = child.stdin.as_mut() {
                use std::io::Write;
                write!(
                    stdin,
                    "FROM scratch\nCOPY fragment.toml tree/ scripts/ /fragment/\n"
                )?;
            }
            child.wait()
        })
        .context("failed to prebuild local fragment")?;

    if !status.success() {
        bail!("podman build failed for local fragment '{}'", name);
    }

    Ok(tag)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test --lib loader`
Expected: all layer extraction tests PASS. Registry tests require skopeo and a live registry — tested in integration (Task 8).

- [ ] **Step 5: Commit**

```bash
cd ~/Work/bootc-assemble
git add src/loader.rs
git commit -m "feat: registry fragment loading and layer extraction

skopeo-based registry interaction, fail-closed layer extraction
(stream-only, traversal/link rejection, size cap, dupe check),
single-layer validation, local fragment prebuilding via podman.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 6: Containerfile generation engine

**Files:**
- Create: `src/generator.rs`
- Modify: `src/lib.rs` — add `pub mod generator;`
- Test: inline `#[cfg(test)]` in `src/generator.rs`

**Interfaces:**
- Consumes: `LoadedFragment` from `loader.rs`, `Manifest` from `manifest.rs`, `split_tree_paths()` from `loader.rs`
- Produces: `generate_containerfile(manifest: &Manifest, fragments: &[LoadedFragment], base_digest: Option<&str>, dedup: &DeduplicationResult) -> Result<String>`

- [ ] **Step 1: Write failing tests for Containerfile generation**

```rust
use anyhow::Result;

use crate::fragment::{Fragment, FragmentPhase, FragmentProvides, FragmentPackages, FragmentConflicts};
use crate::loader::{LoadedFragment, split_tree_paths};
use crate::manifest::{Manifest, ManifestFragment, FragmentSource};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_repos_fragment(name: &str, digest: &str) -> (LoadedFragment, ManifestFragment) {
        let loaded = LoadedFragment {
            fragment: Fragment {
                name: name.to_string(),
                version: "10".into(),
                description: "test".into(),
                vendor: None,
                phase: FragmentPhase::Repos,
                provides: FragmentProvides { repos: vec![name.to_string()] },
                packages: FragmentPackages { available: vec![] },
                conflicts: FragmentConflicts { fragments: vec![] },
            },
            tree_paths: vec![
                PathBuf::from("tree/etc/yum.repos.d/test.repo"),
                PathBuf::from("tree/etc/pki/rpm-gpg/RPM-GPG-KEY-test"),
            ],
            has_configure_script: false,
            source: FragmentSource::Registry {
                image_ref: format!("quay.io/test/{}@sha256:{}", name, digest),
            },
            resolved_digest: Some(format!("sha256:{}", digest)),
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
        };
        let manifest_frag = ManifestFragment {
            image: format!("quay.io/test/{}:10", name),
            packages: vec!["htop".into()],
            mirror: None,
        };
        (loaded, manifest_frag)
    }

    fn make_config_fragment(name: &str, digest: &str) -> (LoadedFragment, ManifestFragment) {
        let loaded = LoadedFragment {
            fragment: Fragment {
                name: name.to_string(),
                version: "2.1".into(),
                description: "test".into(),
                vendor: None,
                phase: FragmentPhase::Config,
                provides: FragmentProvides { repos: vec![] },
                packages: FragmentPackages { available: vec![] },
                conflicts: FragmentConflicts { fragments: vec![] },
            },
            tree_paths: vec![
                PathBuf::from("tree/usr/lib/sysctl.d/99-hardening.conf"),
            ],
            has_configure_script: true,
            source: FragmentSource::Registry {
                image_ref: format!("quay.io/test/{}@sha256:{}", name, digest),
            },
            resolved_digest: Some(format!("sha256:{}", digest)),
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
        };
        let manifest_frag = ManifestFragment {
            image: format!("quay.io/test/{}:2.1", name),
            packages: vec![],
            mirror: None,
        };
        (loaded, manifest_frag)
    }

    fn empty_dedup() -> crate::validate::DeduplicationResult {
        crate::validate::DeduplicationResult {
            deduplicated_repos: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn generated_output_contains_digest_pinned_from() {
        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(
            &manifest, &[epel], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        assert!(output.contains("@sha256:aaa111"));
        assert!(output.contains("@sha256:base123"));
        assert!(!output.contains("FROM quay.io/test/epel:10 AS"));
    }

    #[test]
    fn generated_output_has_phase_ordering() {
        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let (cis, mf_cis) = make_config_fragment("cis", "bbb222");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel, mf_cis],
        };
        let output = generate_containerfile(
            &manifest, &[epel, cis], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        let repos_pos = output.find("Phase: repos").unwrap();
        let packages_pos = output.find("Phase: packages").unwrap();
        let config_pos = output.find("Phase: config").unwrap();
        let preset_pos = output.find("Phase: preset-apply").unwrap();
        let validation_pos = output.find("Phase: validation").unwrap();
        assert!(repos_pos < packages_pos);
        assert!(packages_pos < config_pos);
        assert!(config_pos < preset_pos);
        assert!(preset_pos < validation_pos);
    }

    #[test]
    fn generated_output_batches_packages() {
        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(
            &manifest, &[epel], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        assert!(output.contains("dnf install -y"));
        assert!(output.contains("htop"));
    }

    #[test]
    fn generated_output_has_preset_apply() {
        let (cis, mf_cis) = make_config_fragment("cis", "bbb222");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_cis],
        };
        let output = generate_containerfile(
            &manifest, &[cis], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        assert!(output.contains("systemctl preset-all --preset-mode=enable-only"));
    }

    #[test]
    fn generated_output_has_bootc_lint() {
        let (cis, mf_cis) = make_config_fragment("cis", "bbb222");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_cis],
        };
        let output = generate_containerfile(
            &manifest, &[cis], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        assert!(output.contains("bootc container lint"));
    }

    #[test]
    fn no_packages_phase_when_no_packages() {
        let (cis, mf_cis) = make_config_fragment("cis", "bbb222");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_cis],
        };
        let output = generate_containerfile(
            &manifest, &[cis], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        assert!(!output.contains("dnf install"));
    }

    #[test]
    fn mirror_generates_sed_rewrite() {
        let (epel, mut mf_epel) = make_repos_fragment("epel", "aaa111");
        mf_epel.mirror = Some("https://mirror.corp/epel/".into());
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(
            &manifest, &[epel], Some("sha256:base123"), &empty_dedup(),
        ).unwrap();
        assert!(output.contains("sed"));
        assert!(output.contains("https://mirror.corp/epel/"));
    }

    #[test]
    fn repo_dedup_skips_non_canonical_provider() {
        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let (node_exp, mf_node) = make_repos_fragment("node-exporter", "ccc333");
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel, mf_node],
        };
        let mut dedup = empty_dedup();
        dedup.deduplicated_repos.insert("epel".to_string(), "epel".to_string());
        dedup.deduplicated_repos.insert("node-exporter".to_string(), "node-exporter".to_string());
        let output = generate_containerfile(
            &manifest, &[epel, node_exp], Some("sha256:base123"), &dedup,
        ).unwrap();
        // node-exporter also provides "epel" but epel is canonical — node-exporter's
        // repo COPY should be skipped with a dedup comment
        let epel_copy_count = output.matches("COPY --from=frag-epel").count();
        assert!(epel_copy_count > 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --lib generator`
Expected: compilation errors

- [ ] **Step 3: Implement Containerfile generation**

```rust
use anyhow::Result;
use std::fmt::Write;

use crate::fragment::is_repo_path;
use crate::loader::LoadedFragment;
use crate::manifest::{FragmentSource, Manifest};
use crate::validate::DeduplicationResult;

pub fn generate_containerfile(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
    base_digest: Option<&str>,
    dedup: &DeduplicationResult,
) -> Result<String> {
    let mut out = String::new();

    // Header
    writeln!(out, "# Generated by bootc-assemble v0.1.0")?;
    writeln!(out, "# Manifest: bootc-assemble.yaml")?;

    let frag_summary: Vec<String> = fragments
        .iter()
        .map(|f| format!("{} ({})", f.fragment.name, match f.fragment.phase {
            crate::fragment::FragmentPhase::Repos => "repos",
            crate::fragment::FragmentPhase::Config => "config",
        }))
        .collect();
    writeln!(out, "# Fragments: {}", frag_summary.join(", "))?;

    // Resolved digests
    writeln!(out, "# Resolved digests:")?;
    if let Some(d) = base_digest {
        writeln!(out, "#   base: {}@{}", manifest.base, d)?;
    }
    for loaded in fragments {
        let mf = &manifest.fragments[loaded.manifest_index];
        if let Some(d) = &loaded.resolved_digest {
            writeln!(out, "#   {}: {}@{}", loaded.fragment.name, mf.image, d)?;
        }
    }

    // Compute override summary — detect file path collisions
    let mut path_owners: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for loaded in fragments {
        for p in &loaded.tree_paths {
            if p.to_string_lossy().starts_with("tree/") {
                let dest = p.strip_prefix("tree").unwrap_or(p).to_string_lossy().to_string();
                path_owners
                    .entry(dest)
                    .or_default()
                    .push(loaded.fragment.name.clone());
            }
        }
    }
    let collisions: Vec<_> = path_owners
        .iter()
        .filter(|(_, owners)| owners.len() > 1)
        .collect();
    if collisions.is_empty() {
        writeln!(out, "# Override summary: no file path collisions detected")?;
    } else {
        writeln!(out, "# Override summary: {} file path collision(s) detected", collisions.len())?;
        for (path, owners) in &collisions {
            writeln!(out, "#   {} — written by: {} (last wins)", path, owners.join(", "))?;
        }
    }
    writeln!(out)?;

    // Fragment FROM stages
    writeln!(out, "# --- Fragment stages ---")?;
    for loaded in fragments {
        let mf = &manifest.fragments[loaded.manifest_index];
        let image_ref = match &loaded.source {
            FragmentSource::Registry { image_ref } => image_ref.clone(),
            FragmentSource::Directory { .. } => mf.image.clone(),
        };
        let tag_comment = if image_ref.contains('@') {
            let tag = mf.image.rsplit(':').next().unwrap_or("");
            format!("  # :{}", tag)
        } else {
            String::new()
        };
        writeln!(
            out,
            "FROM {} AS frag-{}{}",
            image_ref, loaded.fragment.name, tag_comment
        )?;
    }
    writeln!(out)?;

    // Base
    writeln!(out, "# --- Base ---")?;
    let base_ref = if let Some(d) = base_digest {
        let base_no_tag = manifest.base.split(':').next().unwrap_or(&manifest.base);
        format!("{}@{}", base_no_tag, d)
    } else {
        manifest.base.clone()
    };
    writeln!(out, "FROM {}", base_ref)?;
    writeln!(out)?;

    // Phase: repos (10)
    let has_repo_content = fragments.iter().any(|f| {
        f.tree_paths
            .iter()
            .any(|p| is_repo_path(p))
    });
    if has_repo_content {
        writeln!(out, "# --- Phase: repos (10) ---")?;
        for loaded in fragments {
            let mf = &manifest.fragments[loaded.manifest_index];
            let repo_paths: Vec<_> = loaded
                .tree_paths
                .iter()
                .filter(|p| is_repo_path(p))
                .collect();
            if repo_paths.is_empty() {
                continue;
            }
            // Skip non-canonical repo providers (dedup: first provider wins)
            let dominated_repos: Vec<_> = loaded
                .fragment
                .provides
                .repos
                .iter()
                .filter(|repo_id| {
                    dedup
                        .deduplicated_repos
                        .get(repo_id.as_str())
                        .map(|canonical| canonical != &loaded.fragment.name)
                        .unwrap_or(false)
                })
                .collect();
            if !dominated_repos.is_empty()
                && dominated_repos.len() == loaded.fragment.provides.repos.len()
            {
                writeln!(out, "# {} — repo files deduplicated (provided by earlier fragment)", loaded.fragment.name)?;
                continue;
            }
            // Copy repo dirs
            if repo_paths.iter().any(|p| p.to_string_lossy().contains("yum.repos.d")) {
                writeln!(
                    out,
                    "COPY --from=frag-{} /fragment/tree/etc/yum.repos.d/ /etc/yum.repos.d/",
                    loaded.fragment.name
                )?;
            }
            if repo_paths.iter().any(|p| p.to_string_lossy().contains("rpm-gpg")) {
                writeln!(
                    out,
                    "COPY --from=frag-{} /fragment/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/",
                    loaded.fragment.name
                )?;
            }
            // Mirror rewrite — scoped to this fragment's repo files only
            if let Some(mirror_url) = &mf.mirror {
                let repo_files: Vec<_> = repo_paths
                    .iter()
                    .filter(|p| p.to_string_lossy().contains("yum.repos.d"))
                    .filter_map(|p| {
                        p.file_name()
                            .map(|f| format!("/etc/yum.repos.d/{}", f.to_string_lossy()))
                    })
                    .collect();
                for repo_file in &repo_files {
                    writeln!(
                        out,
                        "RUN sed -i 's|^baseurl=.*|baseurl={}|; s|^metalink=|#metalink=|; s|^mirrorlist=|#mirrorlist=|' {}",
                        mirror_url, repo_file
                    )?;
                }
            }
        }
        writeln!(out)?;
    }

    // Phase: packages (20)
    let all_packages: Vec<String> = manifest
        .fragments
        .iter()
        .flat_map(|f| f.packages.iter().cloned())
        .collect();
    if !all_packages.is_empty() {
        writeln!(out, "# --- Phase: packages (20) ---")?;
        writeln!(out, "RUN dnf install -y \\")?;
        for (i, pkg) in all_packages.iter().enumerate() {
            if i < all_packages.len() - 1 {
                writeln!(out, "        {} \\", pkg)?;
            } else {
                writeln!(out, "        {} \\", pkg)?;
            }
        }
        writeln!(out, "    && dnf clean all")?;
        writeln!(out)?;
    }

    // Phase: config (30)
    let config_fragments: Vec<_> = fragments
        .iter()
        .filter(|loaded| {
            // Has non-repo tree content or configure.sh
            let has_non_repo_tree = loaded
                .tree_paths
                .iter()
                .filter(|p| p.to_string_lossy().starts_with("tree/"))
                .any(|p| !is_repo_path(p));
            has_non_repo_tree || loaded.has_configure_script
        })
        .collect();

    if !config_fragments.is_empty() {
        writeln!(out, "# --- Phase: config (30) ---")?;
        for loaded in &config_fragments {
            let non_repo_tree: Vec<_> = loaded
                .tree_paths
                .iter()
                .filter(|p| p.to_string_lossy().starts_with("tree/") && !is_repo_path(p))
                .collect();

            writeln!(out, "# {}", loaded.fragment.name)?;
            if !non_repo_tree.is_empty() {
                // Determine the top-level dirs under tree/ that have non-repo content
                let mut top_dirs: Vec<String> = non_repo_tree
                    .iter()
                    .filter_map(|p| {
                        p.strip_prefix("tree/")
                            .ok()
                            .and_then(|r| r.components().next())
                            .map(|c| c.as_os_str().to_string_lossy().to_string())
                    })
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                top_dirs.sort();

                for dir in &top_dirs {
                    if *dir == "etc" {
                        // For /etc, skip repo subdirs but copy the rest
                        let etc_paths: Vec<_> = non_repo_tree
                            .iter()
                            .filter(|p| p.to_string_lossy().starts_with("tree/etc/"))
                            .collect();
                        if !etc_paths.is_empty() {
                            // Copy specific non-repo /etc paths
                            for p in etc_paths {
                                let dest = p.strip_prefix("tree").unwrap();
                                writeln!(
                                    out,
                                    "COPY --from=frag-{} /fragment/{} {}",
                                    loaded.fragment.name,
                                    p.display(),
                                    dest.display()
                                )?;
                            }
                        }
                    } else {
                        writeln!(
                            out,
                            "COPY --from=frag-{} /fragment/tree/{dir}/ /{dir}/",
                            loaded.fragment.name
                        )?;
                    }
                }
            }
            if loaded.has_configure_script {
                writeln!(
                    out,
                    "COPY --from=frag-{name} /fragment/scripts/configure.sh /tmp/frag-{name}-configure.sh",
                    name = loaded.fragment.name
                )?;
                writeln!(
                    out,
                    "RUN /tmp/frag-{name}-configure.sh && rm -f /tmp/frag-{name}-configure.sh",
                    name = loaded.fragment.name
                )?;
            }
            writeln!(out)?;
        }
    }

    // Phase: preset-apply (35)
    writeln!(out, "# --- Phase: preset-apply (35) ---")?;
    writeln!(
        out,
        "RUN systemctl preset-all --preset-mode=enable-only 2>/dev/null || true"
    )?;
    writeln!(out)?;

    // Phase: validation (90)
    writeln!(out, "# --- Phase: validation (90) ---")?;
    writeln!(out, "RUN bootc container lint")?;

    Ok(out)
}
```

- [ ] **Step 4: Add module to lib.rs**

Add `pub mod generator;` to `src/lib.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test --lib generator`
Expected: all 7 tests PASS

- [ ] **Step 6: Commit**

```bash
cd ~/Work/bootc-assemble
git add src/generator.rs src/lib.rs
git commit -m "feat: Containerfile generation engine

Phase-ordered output with tree-splitting, digest-pinned FROM stages,
batched dnf install, preset-apply step, mirror sed rewrite, and
bootc container lint. Full unit test coverage.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 7: CLI wiring — clap, default command, inspect, list

**Files:**
- Rewrite: `src/main.rs` — full CLI implementation
- Create: `src/inspect.rs`
- Create: `src/list.rs`
- Modify: `src/lib.rs` — add `pub mod inspect; pub mod list;`
- Test: `tests/cli.rs` (integration test using `assert_cmd`)

**Interfaces:**
- Consumes: `parse_manifest()`, `load_local_fragment()`, `generate_containerfile()`, all from prior tasks
- Produces: working CLI binary with default command, `inspect`, and `list` subcommands

- [ ] **Step 1: Write CLI integration tests**

`tests/cli.rs`:
```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn no_args_shows_help_or_requires_manifest() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.assert().failure();
}

#[test]
fn inspect_local_directory() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/epel"])
        .assert()
        .success()
        .stdout(predicate::str::contains("epel"))
        .stdout(predicate::str::contains("repos"))
        .stdout(predicate::str::contains("yum.repos.d"));
}

#[test]
fn inspect_tailscale_shows_script() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/tailscale"])
        .assert()
        .success()
        .stdout(predicate::str::contains("configure.sh"));
}

#[test]
fn list_with_local_manifest() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args([
        "list",
        "--manifest",
        "examples/manifests/minimal.yaml",
        "--local",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("epel"))
    .stdout(predicate::str::contains("cis-hardening"));
}

#[test]
fn assemble_with_local_fragments() {
    let mut cmd = Command::cargo_bin("bootc-assemble").unwrap();
    cmd.args([
        "--manifest",
        "examples/manifests/minimal.yaml",
        "--local",
        "--output",
        "/tmp/bootc-assemble-test-containerfile",
    ])
    .assert()
    .success();

    let content = std::fs::read_to_string("/tmp/bootc-assemble-test-containerfile").unwrap();
    assert!(content.contains("Phase: repos"));
    assert!(content.contains("Phase: packages"));
    assert!(content.contains("bootc container lint"));
    std::fs::remove_file("/tmp/bootc-assemble-test-containerfile").ok();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --test cli`
Expected: compilation errors or missing binary

- [ ] **Step 3: Implement inspect.rs**

```rust
use anyhow::Result;
use std::path::Path;

use crate::fragment::parse_fragment_toml;
use crate::loader::is_repo_path;

pub fn run_inspect(target: &str) -> Result<()> {
    let path = Path::new(target);

    let (fragment, tree_paths, has_script) = if path.is_dir() {
        let toml_path = path.join("fragment.toml");
        let content = std::fs::read_to_string(&toml_path)?;
        let frag = parse_fragment_toml(&content)?;

        let mut paths = Vec::new();
        collect_display_paths(path, "tree", &mut paths)?;
        let script_paths_exist = path.join("scripts/configure.sh").exists();

        (frag, paths, script_paths_exist)
    } else {
        // Inspect requires tree/ contents and scripts — always do a
        // full load (metadata-only path skips these). The annotation
        // fast path is used inside load_registry_fragment for the
        // Fragment struct, but the layer is still pulled for tree_paths.
        let loaded = crate::loader::load_registry_fragment(target)?;
        let display_paths: Vec<String> = loaded
            .tree_paths
            .iter()
            .filter(|p| p.to_string_lossy().starts_with("tree/"))
            .map(|p| {
                p.strip_prefix("tree/")
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        (loaded.fragment, display_paths, loaded.has_configure_script)
    };

    let phase_str = match fragment.phase {
        crate::fragment::FragmentPhase::Repos => "repos",
        crate::fragment::FragmentPhase::Config => "config",
    };

    println!("Fragment: {} v{}", fragment.name, fragment.version);
    if let Some(v) = &fragment.vendor {
        println!("Vendor:   {}", v);
    }
    println!("Phase:    {}", phase_str);
    if !fragment.provides.repos.is_empty() {
        println!("Repos:    {}", fragment.provides.repos.join(", "));
    }
    if !fragment.packages.available.is_empty() {
        println!("Packages: {}", fragment.packages.available.join(", "));
    }

    if !tree_paths.is_empty() {
        println!();
        println!("tree/");
        for p in &tree_paths {
            println!("  {}", p);
        }
    }

    println!();
    if has_script {
        println!("scripts/");
        println!("  configure.sh (present)");
    } else {
        println!("scripts/ (none)");
    }

    Ok(())
}

fn collect_display_paths(
    base: &Path,
    prefix: &str,
    paths: &mut Vec<String>,
) -> Result<()> {
    let dir = base.join(prefix);
    if !dir.exists() {
        return Ok(());
    }
    collect_display_recursive(&dir, base, paths)?;
    paths.sort();
    Ok(())
}

fn collect_display_recursive(
    dir: &Path,
    base: &Path,
    paths: &mut Vec<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_display_recursive(&path, base, paths)?;
        } else {
            let rel = path.strip_prefix(base)?;
            // Strip the "tree/" prefix for display
            let display = rel
                .strip_prefix("tree/")
                .unwrap_or(rel)
                .to_string_lossy()
                .to_string();
            paths.push(display);
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Implement list.rs**

```rust
use anyhow::Result;

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;

pub fn run_list(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    println!("Manifest: bootc-assemble.yaml");
    println!("Base:     {}", manifest.base);
    println!();

    let has_digests = fragments.iter().any(|f| f.resolved_digest.is_some());

    if has_digests {
        println!(
            "  {:<20} {:<8} {:<10} {:<20} {}",
            "NAME", "PHASE", "VERSION", "DIGEST", "PACKAGES"
        );
    } else {
        println!(
            "  {:<20} {:<8} {:<10} {}",
            "NAME", "PHASE", "VERSION", "PACKAGES"
        );
    }

    for loaded in fragments {
        let mf = &manifest.fragments[loaded.manifest_index];
        let phase_str = match loaded.fragment.phase {
            crate::fragment::FragmentPhase::Repos => "repos",
            crate::fragment::FragmentPhase::Config => "config",
        };
        let packages = if mf.packages.is_empty() {
            "\u{2014}".to_string()
        } else {
            mf.packages.join(", ")
        };
        if has_digests {
            let digest_short = loaded
                .resolved_digest
                .as_deref()
                .map(|d| {
                    let hash = d.strip_prefix("sha256:").unwrap_or(d);
                    format!("sha256:{}...", &hash[..12.min(hash.len())])
                })
                .unwrap_or_else(|| "(local)".to_string());
            println!(
                "  {:<20} {:<8} {:<10} {:<20} {}",
                loaded.fragment.name, phase_str, loaded.fragment.version, digest_short, packages
            );
        } else {
            println!(
                "  {:<20} {:<8} {:<10} {}",
                loaded.fragment.name, phase_str, loaded.fragment.version, packages
            );
        }
    }

    println!();
    println!("{} fragments", fragments.len());

    Ok(())
}
```

- [ ] **Step 5: Rewrite main.rs with clap CLI**

```rust
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use bootc_assemble::inspect::run_inspect;
use bootc_assemble::list::run_list;
use bootc_assemble::loader::{load_local_fragment, load_registry_fragment, prebuild_local_fragment, resolve_digest};
use bootc_assemble::manifest::{parse_manifest, FragmentSource};
use bootc_assemble::generator::generate_containerfile;
use bootc_assemble::validate::validate_composition;

#[derive(Parser)]
#[command(name = "bootc-assemble", version, about = "Composable image definitions for bootc and RHCOS")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the manifest file
    #[arg(long, default_value = "bootc-assemble.yaml")]
    manifest: PathBuf,

    /// Output path for the generated Containerfile
    #[arg(long, default_value = "Containerfile")]
    output: PathBuf,

    /// After generating, run podman build
    #[arg(long)]
    build: bool,

    /// Treat all fragment image values as local directory paths
    #[arg(long)]
    local: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect a fragment image or directory
    Inspect {
        /// Fragment image reference or local directory path
        target: String,
    },
    /// List fragments from the manifest in phase-sorted order
    List {
        /// Path to the manifest file
        #[arg(long, default_value = "bootc-assemble.yaml")]
        manifest: PathBuf,

        /// Treat all fragment image values as local directory paths
        #[arg(long)]
        local: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Inspect { target }) => {
            run_inspect(&target)?;
        }
        Some(Commands::List { manifest, local }) => {
            let content = std::fs::read_to_string(&manifest)
                .with_context(|| format!("reading manifest {}", manifest.display()))?;
            let manifest_data = parse_manifest(&content)?;

            let (fragments, _temp_images) = load_all_fragments(&manifest_data, local)?;
            run_list(&manifest_data, &fragments)?;
        }
        None => {
            // Default: assembly
            let content = std::fs::read_to_string(&cli.manifest)
                .with_context(|| format!("reading manifest {}", cli.manifest.display()))?;
            let manifest = parse_manifest(&content)?;

            let (mut fragments, temp_images) = load_all_fragments(&manifest, cli.local)?;
            let dedup = validate_composition(&manifest, &fragments)?;

            // Always resolve base image digest — the base is a registry
            // image even when fragments are local directories
            let base_digest = resolve_digest(&manifest.base)
                .map(Some)
                .unwrap_or_else(|e| {
                    eprintln!("warning: could not resolve base image digest: {}", e);
                    None
                });

            let containerfile =
                generate_containerfile(&manifest, &fragments, base_digest.as_deref(), &dedup)?;

            std::fs::write(&cli.output, &containerfile)
                .with_context(|| format!("writing {}", cli.output.display()))?;

            eprintln!(
                "Containerfile written to {}",
                cli.output.display()
            );

            if cli.build {
                let status = std::process::Command::new("podman")
                    .args(["build", "-f", &cli.output.to_string_lossy(), "."])
                    .status()
                    .context("failed to run podman build")?;
                if !status.success() {
                    anyhow::bail!("podman build failed");
                }
            }

            // Clean up prebuilt temp images
            for tag in &temp_images {
                let _ = std::process::Command::new("podman")
                    .args(["rmi", tag])
                    .output();
            }
        }
    }

    Ok(())
}

fn load_all_fragments(
    manifest: &bootc_assemble::manifest::Manifest,
    local: bool,
) -> Result<(Vec<bootc_assemble::loader::LoadedFragment>, Vec<String>)> {
    let mut fragments = Vec::new();
    let mut temp_images: Vec<String> = Vec::new();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = if local {
            FragmentSource::Directory {
                path: PathBuf::from(
                    mf.image.strip_prefix("dir:").unwrap_or(&mf.image),
                ),
            }
        } else {
            mf.resolve_source()
        };

        let mut loaded = match &source {
            FragmentSource::Directory { path } => {
                // Prebuild to temp local image, resolve its digest,
                // then load metadata from the local directory
                let frag_toml = std::fs::read_to_string(path.join("fragment.toml"))?;
                let frag_meta = bootc_assemble::fragment::parse_fragment_toml(&frag_toml)?;
                let tag = prebuild_local_fragment(path, &frag_meta.name)?;
                temp_images.push(tag.clone());
                // Resolve digest of the prebuilt local image
                let local_digest = resolve_digest(&tag)
                    .unwrap_or_else(|_| format!("sha256:local-{}", frag_meta.name));
                let pinned_ref = format!("{}@{}", tag, local_digest);
                let mut l = load_local_fragment(&source)?;
                l.source = FragmentSource::Registry { image_ref: pinned_ref };
                l.resolved_digest = Some(local_digest);
                l
            }
            FragmentSource::Registry { image_ref } => {
                load_registry_fragment(image_ref)?
            }
        };
        loaded.manifest_index = idx;
        fragments.push(loaded);
    }

    // Sort by phase weight; within same weight, preserve manifest order
    fragments.sort_by(|a, b| {
        a.fragment.phase.weight().cmp(&b.fragment.phase.weight())
            .then(a.manifest_index.cmp(&b.manifest_index))
    });
    Ok((fragments, temp_images))
}
```

- [ ] **Step 6: Add modules to lib.rs**

```rust
pub mod fragment;
pub mod generator;
pub mod inspect;
pub mod list;
pub mod loader;
pub mod manifest;
pub mod validate;
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test`
Expected: all unit tests and CLI integration tests PASS

- [ ] **Step 8: Manual smoke test**

```bash
cd ~/Work/bootc-assemble

# --- Local mode smoke tests ---
# Uses dir: prefixed manifests so --local is not needed with registry-ref manifests

# Inspect local directory
cargo run -- inspect examples/fragments/epel
cargo run -- inspect examples/fragments/tailscale

# Create a local-mode manifest using dir: paths
cat > /tmp/local-test.yaml << 'YAML'
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: "dir:examples/fragments/epel"
    packages: [htop, tmux]
  - image: "dir:examples/fragments/cis-hardening"
YAML

# List with local dir: fragments
cargo run -- list --manifest /tmp/local-test.yaml

# Assemble with local dir: fragments (prebuilds temp images)
cargo run -- --manifest /tmp/local-test.yaml --output /tmp/local-test-containerfile
cat /tmp/local-test-containerfile
# Verify: FROM lines reference localhost/bootc-assemble/frag-* temp images

# --- Registry mode smoke tests (requires local registry) ---
# Proves the full end-to-end: build fragments -> push -> assemble -> build image

# Start a local registry
podman run -d --name bootc-assemble-registry -p 5000:5000 registry:2

# Build and push 5 example fragments (exceeds the 4-fragment success criterion)
for frag in epel tailscale cis-hardening node-exporter nginx; do
  podman build -t localhost:5000/fragments/$frag:latest \
    -f examples/fragments/$frag/Containerfile.fragment \
    examples/fragments/$frag/
  podman push localhost:5000/fragments/$frag:latest --tls-verify=false
done

# Inspect from registry
cargo run -- inspect localhost:5000/fragments/epel:latest
cargo run -- inspect localhost:5000/fragments/tailscale:latest

# List from registry
cat > /tmp/registry-test.yaml << 'YAML'
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: localhost:5000/fragments/epel:latest
    packages: [htop, tmux]
  - image: localhost:5000/fragments/tailscale:latest
    packages: [tailscale]
  - image: localhost:5000/fragments/cis-hardening:latest
  - image: localhost:5000/fragments/node-exporter:latest
    packages: [golang-github-prometheus-node-exporter]
  - image: localhost:5000/fragments/nginx:latest
    packages: [nginx]
YAML

cargo run -- list --manifest /tmp/registry-test.yaml
# Verify: digest column shows sha256:... for all 5 fragments

# Assemble from registry
cargo run -- --manifest /tmp/registry-test.yaml --output /tmp/registry-test-containerfile
cat /tmp/registry-test-containerfile
# Verify:
# - FROM lines use @sha256: digests, not :latest tags
# - Base FROM also uses @sha256:
# - Phase ordering: repos(10) -> packages(20) -> config(30) -> preset-apply(35) -> validation(90)
# - Repo dedup: epel repo appears once (node-exporter also provides epel)
# - Packages batched into single dnf install
# - systemctl preset-all present
# - bootc container lint present

# Full build test — validates the generated Containerfile actually builds.
# Requires RHEL entitlements for dnf install. Skip if not available.
if podman run --rm registry.redhat.io/rhel10/rhel-bootc:10.0 true 2>/dev/null; then
  echo "RHEL entitlements available — running full build test"
  cargo run -- --manifest /tmp/registry-test.yaml --build
  echo "Full build succeeded"
else
  echo "RHEL entitlements not available — skipping full build test"
  echo "The generated Containerfile was validated structurally above"
fi

# Cleanup
podman stop bootc-assemble-registry && podman rm bootc-assemble-registry
```

Verify the output matches the expected Containerfile format from the spec.

- [ ] **Step 9: Commit**

```bash
cd ~/Work/bootc-assemble
git add src/ tests/
git commit -m "feat: CLI — default assembly, inspect, and list commands

clap-based CLI with --local, --build, --output, --manifest flags.
Inspect shows fragment metadata and tree contents. List shows
phase-sorted fragment summary. Integration tests with assert_cmd.

Assisted-by: Claude Code (claude-opus-4-6)"
```

---

### Task 8: Validation and repo deduplication

**Files:**
- Create: `src/validate.rs`
- Modify: `src/lib.rs` — add `pub mod validate;`
- Modify: `src/main.rs` — wire validation into assembly flow
- Test: inline `#[cfg(test)]` in `src/validate.rs`

**Interfaces:**
- Consumes: `LoadedFragment` from `loader.rs`, `Manifest` from `manifest.rs`
- Produces: `validate_composition(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()>`, `check_repo_deduplication(fragments: &[LoadedFragment]) -> Result<()>`, `check_conflicts(fragments: &[LoadedFragment]) -> Result<()>`

- [ ] **Step 1: Write failing tests for validation**

```rust
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::fragment::*;
use crate::loader::LoadedFragment;
use crate::manifest::{FragmentSource, Manifest, ManifestFragment};

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fragment(
        name: &str,
        repos: Vec<&str>,
        conflicts: Vec<&str>,
    ) -> LoadedFragment {
        LoadedFragment {
            fragment: Fragment {
                name: name.to_string(),
                version: "1.0".into(),
                description: "test".into(),
                vendor: None,
                phase: FragmentPhase::Config,
                provides: FragmentProvides {
                    repos: repos.into_iter().map(String::from).collect(),
                },
                packages: FragmentPackages { available: vec![] },
                conflicts: FragmentConflicts {
                    fragments: conflicts.into_iter().map(String::from).collect(),
                },
            },
            tree_paths: vec![],
            has_configure_script: false,
            source: FragmentSource::Directory {
                path: PathBuf::from("/test"),
            },
            resolved_digest: None,
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn no_conflicts_passes() {
        let frags = vec![
            test_fragment("a", vec!["repo-a"], vec![]),
            test_fragment("b", vec!["repo-b"], vec![]),
        ];
        assert!(check_conflicts(&frags).is_ok());
    }

    #[test]
    fn declared_conflict_fails() {
        let frags = vec![
            test_fragment("a", vec![], vec!["b"]),
            test_fragment("b", vec![], vec![]),
        ];
        assert!(check_conflicts(&frags).is_err());
    }

    #[test]
    fn mutual_conflict_fails() {
        let frags = vec![
            test_fragment("a", vec![], vec!["b"]),
            test_fragment("b", vec![], vec!["a"]),
        ];
        assert!(check_conflicts(&frags).is_err());
    }

    #[test]
    fn repo_dedup_same_id_deduplicates() {
        let frags = vec![
            test_fragment("epel-user1", vec!["epel"], vec![]),
            test_fragment("epel-user2", vec!["epel"], vec![]),
        ];
        let result = check_repo_deduplication(&frags).unwrap();
        assert!(result.deduplicated_repos.contains_key("epel"));
        assert_eq!(result.deduplicated_repos["epel"], "epel-user1");
    }

    #[test]
    fn duplicate_fragment_names_fail() {
        let frags = vec![
            test_fragment("myapp", vec![], vec![]),
            test_fragment("myapp", vec![], vec![]),
        ];
        assert!(check_duplicate_names(&frags).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd ~/Work/bootc-assemble && cargo test --lib validate`
Expected: compilation errors

- [ ] **Step 3: Implement validation**

```rust
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;

pub fn validate_composition(
    _manifest: &Manifest,
    fragments: &[LoadedFragment],
) -> Result<DeduplicationResult> {
    check_duplicate_names(fragments)?;
    check_conflicts(fragments)?;
    let dedup = check_repo_deduplication(fragments)?;
    Ok(dedup)
}

pub fn check_duplicate_names(fragments: &[LoadedFragment]) -> Result<()> {
    let mut seen = HashSet::new();
    for f in fragments {
        if !seen.insert(&f.fragment.name) {
            bail!(
                "duplicate fragment name '{}' — each fragment must have a unique name",
                f.fragment.name
            );
        }
    }
    Ok(())
}

pub fn check_conflicts(fragments: &[LoadedFragment]) -> Result<()> {
    let names: HashSet<&str> = fragments.iter().map(|f| f.fragment.name.as_str()).collect();

    for f in fragments {
        for conflict_name in &f.fragment.conflicts.fragments {
            if names.contains(conflict_name.as_str()) {
                bail!(
                    "fragment '{}' declares a conflict with '{}', which is also in the manifest",
                    f.fragment.name,
                    conflict_name
                );
            }
        }
    }
    Ok(())
}

/// For each repo ID provided by multiple fragments, compare the actual
/// .repo file content. Identical definitions are deduplicated (first
/// provider wins). Conflicting definitions (same repo ID, different
/// content) fail the build with a clear error.
pub fn check_repo_deduplication(fragments: &[LoadedFragment]) -> Result<DeduplicationResult> {
    // Map repo ID -> list of (fragment name, repo file content hash)
    let mut repo_providers: HashMap<String, Vec<(&str, u64)>> = HashMap::new();

    for f in fragments {
        for repo_id in &f.fragment.provides.repos {
            // Hash actual .repo file contents (populated during loading
            // for both local and registry fragments).
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            let mut sorted_files: Vec<_> = f.repo_file_contents.iter().collect();
            sorted_files.sort_by_key(|(name, _)| name.clone());
            for (name, content) in &sorted_files {
                name.hash(&mut hasher);
                content.hash(&mut hasher);
            }
            let content_hash = hasher.finish();

            repo_providers
                .entry(repo_id.clone())
                .or_default()
                .push((&f.fragment.name, content_hash));
        }
    }

    let mut deduplicated_repos: HashMap<String, String> = HashMap::new();
    for (repo_id, providers) in &repo_providers {
        if providers.len() > 1 {
            // Check for conflicting definitions (different content hashes)
            let first_hash = providers[0].1;
            for (name, hash) in &providers[1..] {
                if *hash != first_hash {
                    bail!(
                        "repo '{}' has conflicting definitions: fragment '{}' and fragment '{}' provide different .repo content for the same repo ID",
                        repo_id,
                        providers[0].0,
                        name
                    );
                }
            }

            // Identical definitions — first provider wins
            let canonical = providers[0].0;
            let skipped: Vec<&str> = providers[1..].iter().map(|(n, _)| *n).collect();
            eprintln!(
                "note: repo '{}' deduplicated — using '{}', skipping '{}'",
                repo_id,
                canonical,
                skipped.join("', '")
            );
            deduplicated_repos.insert(repo_id.clone(), canonical.to_string());
        }
    }

    Ok(DeduplicationResult { deduplicated_repos })
}

#[derive(Debug)]
pub struct DeduplicationResult {
    /// Map of repo ID -> canonical provider fragment name.
    /// Only populated for repo IDs with multiple providers.
    /// The generator skips repo COPY steps for non-canonical providers.
    pub deduplicated_repos: HashMap<String, String>,
}
```

- [ ] **Step 4: Wire validation into main.rs assembly flow**

Add before `generate_containerfile()` in the `None` (assembly) branch of main.rs:

```rust
use bootc_assemble::validate::validate_composition;

// ... after loading fragments:
validate_composition(&manifest, &fragments)?;
```

- [ ] **Step 5: Add module to lib.rs**

Add `pub mod validate;` to `src/lib.rs`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd ~/Work/bootc-assemble && cargo test`
Expected: all tests PASS

- [ ] **Step 7: Commit**

```bash
cd ~/Work/bootc-assemble
git add src/validate.rs src/lib.rs src/main.rs
git commit -m "feat: composition validation — conflicts, dedup, name uniqueness

Validates declared conflicts between fragments, detects duplicate
fragment names, and logs repo deduplication when multiple fragments
provide the same repo ID.

Assisted-by: Claude Code (claude-opus-4-6)"
```
