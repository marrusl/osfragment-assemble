use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FragmentProvides {
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FragmentPackages {
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FragmentConflicts {
    #[serde(default)]
    pub fragments: Vec<String>,
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
    pub provides: FragmentProvides,
    pub packages: FragmentPackages,
    pub conflicts: FragmentConflicts,
}

pub fn parse_fragment_toml(content: &str) -> Result<Fragment> {
    let parsed: FragmentToml = toml::from_str(content).context("failed to parse fragment.toml")?;
    let inner = parsed.fragment;
    Ok(Fragment {
        name: inner.name,
        version: inner.version,
        description: inner.description,
        vendor: inner.vendor,
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


#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[fragment]
name = "epel"
version = "10"
description = "EPEL repository for RHEL"

[fragment.provides]
repos = ["epel"]
"#;

    const FULL_TOML: &str = r#"
[fragment]
name = "tailscale"
version = "1.82.0"
description = "Tailscale VPN client"
vendor = "Tailscale Inc."

[fragment.provides]
repos = ["tailscale-stable"]

[fragment.packages]
required = ["tailscale"]

[fragment.conflicts]
fragments = []
"#;

    #[test]
    fn parse_minimal_fragment() {
        let frag = parse_fragment_toml(MINIMAL_TOML).unwrap();
        assert_eq!(frag.name, "epel");
        assert_eq!(frag.version, "10");
        assert_eq!(frag.provides.repos, vec!["epel"]);
        assert!(frag.vendor.is_none());
        assert!(frag.packages.required.is_empty());
    }

    #[test]
    fn parse_full_fragment() {
        let frag = parse_fragment_toml(FULL_TOML).unwrap();
        assert_eq!(frag.name, "tailscale");
        assert_eq!(frag.vendor.as_deref(), Some("Tailscale Inc."));
        assert_eq!(frag.packages.required, vec!["tailscale"]);
    }

    /// A fragment published before `phase` was removed still carries the key.
    /// `FragmentInner` does not deny unknown fields, so such a fragment parses
    /// and the stale key is ignored rather than rejected. Previously published
    /// fragments therefore keep working without a rebuild.
    #[test]
    fn stale_phase_key_is_ignored() {
        let stale = r#"
[fragment]
name = "legacy"
version = "1"
description = "published before phase was removed"
phase = "repos"
"#;
        let frag = parse_fragment_toml(stale).expect("stale phase key must not be rejected");
        assert_eq!(frag.name, "legacy");
    }

    #[test]
    fn reject_unknown_packages_field() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "unknown field test"

[fragment.packages]
available = ["grafana"]
"#;
        let err = parse_fragment_toml(bad).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' error, got: {}",
            msg
        );
    }

    #[test]
    fn reject_typo_in_packages_field() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "typo field test"

[fragment.packages]
requred = ["grafana"]
"#;
        let err = parse_fragment_toml(bad).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' error, got: {}",
            msg
        );
    }

    #[test]
    fn parse_all_example_fragments() {
        let examples_dir = Path::new("examples/fragments");
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

    #[test]
    fn postgresql_example_declares_required_packages() {
        let toml_path = Path::new("examples/fragments/postgresql/fragment.toml");
        let content =
            std::fs::read_to_string(toml_path).expect("postgresql example fragment should exist");
        let frag =
            parse_fragment_toml(&content).expect("postgresql example fragment should parse");
        assert!(
            frag.packages.required.contains(&"postgresql17-server".to_string()),
            "postgresql must declare postgresql17-server as required"
        );
        assert!(
            frag.packages.required.contains(&"postgresql17".to_string()),
            "postgresql must declare postgresql17 as required"
        );
    }
}
