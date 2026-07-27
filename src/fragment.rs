use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FragmentProvides {
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FragmentPackages {
    #[serde(default)]
    pub available: Vec<String>,
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
    let parsed: FragmentToml = toml::from_str(content).context("failed to parse fragment.toml")?;
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
}
