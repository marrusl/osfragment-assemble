use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::path::{Path, PathBuf};

use crate::fragment::{
    is_repo_path, parse_fragment_toml, validate_phase_consistency, Fragment, FragmentConflicts,
    FragmentPackages, FragmentPhase, FragmentProvides,
};
use crate::generator::split_image_ref;
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
    let repo: Vec<_> = tree_paths
        .iter()
        .filter(|p| is_repo_path(p))
        .cloned()
        .collect();
    let config: Vec<_> = tree_paths
        .iter()
        .filter(|p| !is_repo_path(p))
        .cloned()
        .collect();
    (repo, config)
}

pub fn resolve_digest(image_ref: &str) -> Result<String> {
    let digest_output = std::process::Command::new("skopeo")
        .args([
            "inspect",
            "--override-os",
            "linux",
            "--format",
            "{{.Digest}}",
            &format!("docker://{}", image_ref),
        ])
        .output()
        .context("failed to run skopeo inspect for digest")?;

    if !digest_output.status.success() {
        let stderr = String::from_utf8_lossy(&digest_output.stderr);
        bail!("skopeo digest lookup failed for {}: {}", image_ref, stderr);
    }

    let digest = String::from_utf8(digest_output.stdout)?.trim().to_string();
    Ok(digest)
}

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
            bail!("path traversal detected in fragment layer: {}", path_str);
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
        if entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link() {
            bail!(
                "symlink or hardlink rejected in fragment layer: {}",
                path_str
            );
        }
        if entry.header().entry_type().is_file() {
            paths.push(path.to_path_buf());
        }
    }
    Ok(paths)
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
        let path_str = path.to_string_lossy().to_string();

        // Fail-closed: reject traversal
        if path_str.contains("..") {
            bail!("path traversal detected in fragment layer: {}", path_str);
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

        if path_str.contains("yum.repos.d/") && path_str.ends_with(".repo") {
            let filename = Path::new(&path_str)
                .file_name()
                .map(|f| f.to_string_lossy().to_string());
            if let Some(filename) = filename {
                let mut content = String::new();
                use std::io::Read;
                entry.read_to_string(&mut content)?;
                contents.insert(filename, content);
            }
        }
    }
    Ok(contents)
}

/// Try the OCI annotation fast path: parse fragment metadata from
/// manifest annotations without pulling any layers.
fn try_annotation_fast_path(image_ref: &str) -> Result<Option<Fragment>> {
    let output = std::process::Command::new("skopeo")
        .args([
            "inspect",
            "--override-os",
            "linux",
            "--raw",
            &format!("docker://{}", image_ref),
        ])
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
    let name = annotations
        .get("io.bootc.fragment.name")
        .and_then(|v| v.as_str());
    let version = annotations
        .get("io.bootc.fragment.version")
        .and_then(|v| v.as_str());
    let description = annotations
        .get("io.bootc.fragment.description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let phase_str = annotations
        .get("io.bootc.fragment.phase")
        .and_then(|v| v.as_str());

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
    let (name, _tag) = split_image_ref(image_ref);
    let image_with_digest = format!("{}@{}", name, digest);

    // Assembly always parses the in-layer fragment.toml for the authoritative
    // Fragment.  The annotation fast path is limited to metadata-only
    // operations (inspect/list via load_registry_fragment_metadata_only)
    // because annotations omit fields like conflicts.
    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let oci_path = tmp.path().join("oci-layout");

    let status = std::process::Command::new("skopeo")
        .args([
            "copy",
            "--override-os",
            "linux",
            &format!("docker://{}", image_with_digest),
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

    let layer_blob_path = oci_path.join("blobs").join(layer_digest.replace(':', "/"));
    let layer_bytes = std::fs::read(&layer_blob_path)?;

    let toml_content = extract_fragment_toml_from_bytes(&layer_bytes)?;
    let fragment = parse_fragment_toml(&toml_content)?;

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

/// Metadata-only registry load for inspect and list.
/// Uses annotations to skip the layer pull entirely when possible.
/// Falls back to full load_registry_fragment when annotations are absent.
/// Note: This fast path is intentionally limited to inspect/list commands.
/// During assembly, conflicts and vendor fields are checked via the full
/// load_registry_fragment path, which always parses the in-layer TOML.
pub fn load_registry_fragment_metadata_only(image_ref: &str) -> Result<LoadedFragment> {
    let digest = resolve_digest(image_ref)?;
    let (name, _tag) = split_image_ref(image_ref);
    let image_with_digest = format!("{}@{}", name, digest);

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

#[cfg(test)]
mod tests {
    use super::*;

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

#[cfg(test)]
mod layer_tests {
    use super::*;

    fn create_test_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for (path, data) in entries {
                let mut header = tar::Header::new_gnu();
                // tar::Header::set_path rejects paths with ".." components.
                // For testing malicious tarballs, write the path directly
                // into the raw header name field to bypass that validation.
                if header.set_path(path).is_err() {
                    let name = &mut header.as_old_mut().name;
                    name.fill(0);
                    let path_bytes = path.as_bytes();
                    let len = path_bytes.len().min(name.len());
                    name[..len].copy_from_slice(&path_bytes[..len]);
                }
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
            (
                "fragment/fragment.toml",
                b"[fragment]\nname=\"x\"\nversion=\"1\"\ndescription=\"x\"\nphase=\"repos\"",
            ),
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
            (
                "fragment/fragment.toml",
                b"[fragment]\nname=\"a\"\nversion=\"1\"\ndescription=\"a\"\nphase=\"repos\"",
            ),
            (
                "fragment/fragment.toml",
                b"[fragment]\nname=\"b\"\nversion=\"2\"\ndescription=\"b\"\nphase=\"config\"",
            ),
        ]);
        let result = extract_fragment_toml_from_bytes(&tarball);
        assert!(result.is_err());
    }
}
