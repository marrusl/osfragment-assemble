use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub enum FragmentSource {
    Registry { image_ref: String },
}

// ManifestYaml fields are read during serde deserialization but not
// accessed directly after parse_manifest transforms them into Manifest.
// The dead_code warning is expected and does not indicate unused code.
#[allow(dead_code)]
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
    pub fn resolve_source(&self) -> Result<FragmentSource> {
        if self.image.starts_with("dir:") {
            bail!("local directory sources (dir:) are not supported — push fragments to a registry first");
        }
        Ok(FragmentSource::Registry {
            image_ref: self.image.clone(),
        })
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

    #[test]
    fn parse_minimal_manifest() {
        let manifest = parse_manifest(MINIMAL_YAML).unwrap();
        assert_eq!(manifest.base, "registry.redhat.io/rhel10/rhel-bootc:10.0");
        assert_eq!(manifest.fragments.len(), 2);
        assert_eq!(manifest.fragments[0].packages, vec!["htop", "tmux"]);
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
        let source = manifest.fragments[0].resolve_source().unwrap();
        assert!(matches!(source, FragmentSource::Registry { .. }));
    }

    #[test]
    fn reject_dir_source() {
        let dir_yaml = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: "dir:./examples/fragments/epel"
"#;
        let manifest = parse_manifest(dir_yaml).unwrap();
        let result = manifest.fragments[0].resolve_source();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("local directory sources"));
        assert!(err_msg.contains("not supported"));
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
