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
