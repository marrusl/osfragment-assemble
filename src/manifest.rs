use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseType {
    Bootc,
    Container,
}

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
    #[serde(default, rename = "baseType")]
    base_type: Option<BaseType>,
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
    pub base_type: Option<BaseType>,
    pub fragments: Vec<ManifestFragment>,
    /// Path the manifest was read from, as the user wrote it. Reported in the
    /// generated Containerfile header and by `list`, so it must be the real
    /// `--manifest` argument rather than a default filename.
    pub source_path: String,
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

pub fn parse_manifest(content: &str, source_path: &str) -> Result<Manifest> {
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

    Ok(Manifest {
        base,
        base_type: raw.base_type,
        fragments,
        source_path: source_path.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: quay.io/marrusl2/fragments/epel:10
    packages:
      - htop
      - tmux
  - image: quay.io/marrusl2/fragments/cis-hardening:2.1
"#;

    const MIRROR_YAML: &str = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: quay.io/marrusl2/fragments/grafana:11.0
    packages:
      - grafana
    mirror: https://rpm-mirror.internal.corp/grafana/
"#;

    #[test]
    fn parse_minimal_manifest() {
        let manifest = parse_manifest(MINIMAL_YAML, "test-manifest.yaml").unwrap();
        assert_eq!(manifest.base, "registry.redhat.io/rhel10/rhel-bootc:10.0");
        assert_eq!(manifest.fragments.len(), 2);
        assert_eq!(manifest.fragments[0].packages, vec!["htop", "tmux"]);
        assert!(manifest.fragments[1].packages.is_empty());
    }

    #[test]
    fn parse_mirror_override() {
        let manifest = parse_manifest(MIRROR_YAML, "test-manifest.yaml").unwrap();
        assert_eq!(
            manifest.fragments[0].mirror.as_deref(),
            Some("https://rpm-mirror.internal.corp/grafana/")
        );
    }

    /// The generated Containerfile's `# Manifest:` header and `list`'s
    /// `Manifest:` line both report provenance from this field, and both
    /// were once hardcoded to the default path. It must carry whatever
    /// path the manifest was actually read from.
    #[test]
    fn parsed_manifest_records_its_source_path() {
        let manifest = parse_manifest(MINIMAL_YAML, "configs/edge-lab.yaml").unwrap();
        assert_eq!(manifest.source_path, "configs/edge-lab.yaml");
    }

    #[test]
    fn resolve_registry_source() {
        let manifest = parse_manifest(MINIMAL_YAML, "test-manifest.yaml").unwrap();
        let source = manifest.fragments[0].resolve_source().unwrap();
        assert!(matches!(source, FragmentSource::Registry { .. }));
    }

    #[test]
    fn reject_dir_source() {
        let dir_yaml = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments:
  - image: "dir:./examples/fragments/epel"
"#;
        let manifest = parse_manifest(dir_yaml, "test-manifest.yaml").unwrap();
        let result = manifest.fragments[0].resolve_source();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("local directory sources"));
        assert!(err_msg.contains("not supported"));
    }

    #[test]
    fn reject_missing_base() {
        let bad = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
fragments:
  - image: quay.io/test:1
"#;
        assert!(parse_manifest(bad, "test-manifest.yaml").is_err());
    }

    #[test]
    fn reject_empty_fragments() {
        let bad = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
fragments: []
"#;
        let result = parse_manifest(bad, "test-manifest.yaml");
        assert!(result.is_err());
    }

    #[test]
    fn parse_base_type_bootc() {
        let yaml = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
baseType: bootc
fragments:
  - image: quay.io/test/epel:10
"#;
        let manifest = parse_manifest(yaml, "test-manifest.yaml").unwrap();
        assert_eq!(manifest.base_type, Some(BaseType::Bootc));
    }

    #[test]
    fn parse_base_type_container() {
        let yaml = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: quay.io/fedora/fedora:41
baseType: container
fragments:
  - image: quay.io/test/epel:10
"#;
        let manifest = parse_manifest(yaml, "test-manifest.yaml").unwrap();
        assert_eq!(manifest.base_type, Some(BaseType::Container));
    }

    #[test]
    fn parse_base_type_absent() {
        let manifest = parse_manifest(MINIMAL_YAML, "test-manifest.yaml").unwrap();
        assert_eq!(manifest.base_type, None);
    }

    #[test]
    fn parse_base_type_invalid_rejected() {
        let yaml = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: quay.io/fedora/fedora:41
baseType: something-else
fragments:
  - image: quay.io/test/epel:10
"#;
        assert!(parse_manifest(yaml, "test-manifest.yaml").is_err());
    }
}
