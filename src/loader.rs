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
    let _manifest: serde_json::Value = serde_json::from_str(&stdout)?;

    // The digest is available from the Docker-Content-Digest header,
    // but skopeo inspect --raw gives us the manifest body.
    // We compute the digest from the manifest bytes.
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

/// Resolve digest for an image in local podman storage (not a registry).
/// Uses `podman image inspect` which reads local container storage directly.
pub fn resolve_local_digest(image_ref: &str) -> Result<String> {
    let output = std::process::Command::new("podman")
        .args(["image", "inspect", "--format", "{{.Digest}}", image_ref])
        .output()
        .context("failed to run podman image inspect")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "podman image inspect failed for {}: {}",
            image_ref, stderr
        );
    }

    let digest = String::from_utf8(output.stdout)?
        .trim()
        .to_string();

    if digest.is_empty() || digest == "<nil>" {
        bail!(
            "no digest available for local image {} — image may not have been built correctly",
            image_ref
        );
    }

    Ok(digest)
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
