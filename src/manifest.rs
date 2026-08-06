use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub enum FragmentSource {
    Registry { image_ref: String },
}

// ManifestYaml fields are read during serde deserialization but not
// accessed directly after parse_manifest transforms them into Manifest.
// The dead_code warning is expected and does not indicate unused code.
//
// deny_unknown_fields: the manifest is user-authored external input, and a
// misspelled or removed key (baseType, once a real field) must fail the
// parse rather than be silently ignored.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestYaml {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    base: Option<String>,
    #[serde(default)]
    fragments: Vec<ManifestFragmentYaml>,
}

// deny_unknown_fields: same reasoning as ManifestYaml. A typo'd key inside
// a fragment entry (mirrorr:) must fail the parse, not be silently ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// `baseType` is no longer a manifest field: the tool targets bootc
    /// bases only, so there is no classification to override. The values it
    /// used to accept must fail the parse like any other unknown key, not
    /// be silently ignored.
    #[test]
    fn base_type_is_rejected_as_an_unknown_field() {
        for value in ["bootc", "container", "something-else"] {
            let yaml = format!(
                r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: quay.io/centos-bootc/centos-bootc:stream10
baseType: {value}
fragments:
  - image: quay.io/test/epel:10
"#
            );
            let err = parse_manifest(&yaml, "test-manifest.yaml")
                .expect_err("baseType must fail the parse");
            let full = format!("{err:#}");
            assert!(
                full.contains("baseType"),
                "error for baseType: {value} must name the field, got: {full}"
            );
        }
    }

    #[test]
    fn unknown_manifest_field_is_rejected() {
        let yaml = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: quay.io/centos-bootc/centos-bootc:stream10
fragmnets:
  - image: quay.io/test/epel:10
"#;
        let full = format!(
            "{:#}",
            parse_manifest(yaml, "test-manifest.yaml").unwrap_err()
        );
        assert!(
            full.contains("fragmnets"),
            "a typo'd key must fail the parse naming the key, got: {full}"
        );
    }

    #[test]
    fn unknown_fragment_entry_field_is_rejected() {
        let yaml = r#"
apiVersion: osfragment/v1alpha1
kind: Composition
base: quay.io/centos-bootc/centos-bootc:stream10
fragments:
  - image: quay.io/test/epel:10
    mirrorr: https://mirror.internal.corp/epel
"#;
        let full = format!(
            "{:#}",
            parse_manifest(yaml, "test-manifest.yaml").unwrap_err()
        );
        assert!(
            full.contains("mirrorr"),
            "a typo'd fragment key must fail the parse naming the key, got: {full}"
        );
    }

    /// `deny_unknown_fields` means any schema change can break a manifest that
    /// used to parse, and the manifests shipped in `examples/` are the ones a
    /// user copies first. They must all still parse.
    #[test]
    fn every_committed_example_manifest_parses() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/manifests");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).expect("examples/manifests must exist") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            parse_manifest(&content, &path.display().to_string())
                .unwrap_or_else(|e| panic!("{} must parse: {e:#}", path.display()));
            checked += 1;
        }
        assert!(checked > 0, "no example manifests found under {dir}");
    }
}
