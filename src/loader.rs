use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::path::{Path, PathBuf};

use crate::fragment::{
    is_repo_path, parse_fragment_toml, Fragment, FragmentConflicts, FragmentPackages,
    FragmentProvides,
};
use crate::generator::split_image_ref;
use crate::manifest::FragmentSource;

#[derive(Debug, Clone)]
pub struct LoadedFragment {
    pub fragment: Fragment,
    pub tree_paths: Vec<PathBuf>,
    pub hook_paths: Vec<PathBuf>,
    pub source: FragmentSource,
    pub resolved_digest: Option<String>,
    /// Index into `manifest.fragments`. Emission is manifest order, so this
    /// currently equals the fragment's position in the slice.
    pub manifest_index: usize,
    /// Cached .repo file contents for repo conflict comparison, keyed by filename.
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

/// Shared validation for tar entries across all extraction functions.
/// Rejects path traversal, absolute paths outside /fragment/, and symlinks/hardlinks.
fn validate_tar_entry(path_str: &str, entry_type: tar::EntryType) -> Result<()> {
    if path_str.contains("..") {
        bail!("path traversal detected in fragment layer: {}", path_str);
    }
    if path_str.starts_with('/') && !path_str.starts_with("/fragment/") {
        bail!(
            "absolute path outside /fragment/ rejected in fragment layer: {}",
            path_str
        );
    }
    if entry_type.is_symlink() || entry_type.is_hard_link() {
        bail!(
            "symlink or hardlink rejected in fragment layer: {}",
            path_str
        );
    }
    Ok(())
}

pub fn extract_fragment_toml_from_bytes(compressed: &[u8]) -> Result<String> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);

    let mut found: Option<String> = None;

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let path = entry.path().context("reading entry path")?;
        let path_str = path.to_string_lossy();

        validate_tar_entry(&path_str, entry.header().entry_type())?;

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

        validate_tar_entry(&path_str, entry.header().entry_type())?;
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

        validate_tar_entry(&path_str, entry.header().entry_type())?;

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

/// Write a layer's `fragment/tree/` and `fragment/hooks/` payload to disk
/// under `dest_dir/tree` and `dest_dir/hooks`. Shares the same tar-entry
/// security validation as the metadata-only extractors above.
///
/// `pub(crate)` rather than private: `src/self_contained.rs`'s tests
/// compose this directly with `create_archive` over a fixture layer to
/// exercise the spec's materialize-then-archive acceptance test without a
/// registry (Task 6). `materialize_fragment` below is still the production
/// entry point.
///
/// Extraction streams entry-by-entry, so on `Err` `dest_dir` may contain
/// partially written files from entries processed before the failing one;
/// callers must treat it as unusable rather than a valid partial result.
pub(crate) fn extract_fragment_payload_to_disk(compressed: &[u8], dest_dir: &Path) -> Result<()> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let path = entry.path().context("reading entry path")?.to_path_buf();
        let path_str = path.to_string_lossy().to_string();

        validate_tar_entry(&path_str, entry.header().entry_type())?;

        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            continue;
        }

        let dest = if let Ok(rel) = path.strip_prefix("fragment/tree") {
            dest_dir.join("tree").join(rel)
        } else if let Ok(rel) = path.strip_prefix("fragment/hooks") {
            dest_dir.join("hooks").join(rel)
        } else {
            continue;
        };

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        entry
            .unpack(&dest)
            .with_context(|| format!("writing {}", dest.display()))?;
    }
    Ok(())
}

/// Pull a fragment image by reference and materialize its tree/hooks
/// payload to disk under `dest_dir`. Reuses `pull_layer_bytes`, the same
/// skopeo-copy-then-walk-layers path `load_registry_fragment` uses; only
/// the sink differs (files on disk instead of an in-memory path list).
///
/// On `Err`, `dest_dir` may contain partially written files from an
/// earlier layer or an earlier entry within the failing layer; callers
/// must treat it as unusable rather than a valid partial result.
pub fn materialize_fragment(image_ref: &str, dest_dir: &Path) -> Result<()> {
    for layer_bytes in pull_layer_bytes(image_ref)? {
        extract_fragment_payload_to_disk(&layer_bytes, dest_dir)?;
    }
    Ok(())
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

    Ok(fragment_from_annotations(annotations))
}

/// Map an OCI manifest `annotations` object onto a `Fragment`.
/// Returns `None` when a required key is missing; the caller falls back
/// to layer extraction.
fn fragment_from_annotations(annotations: &serde_json::Value) -> Option<Fragment> {
    // Check for required annotation fields
    let name = annotations
        .get("com.github.marrusl.osfragment.name")
        .and_then(|v| v.as_str());
    let version = annotations
        .get("com.github.marrusl.osfragment.version")
        .and_then(|v| v.as_str());
    let description = annotations
        .get("com.github.marrusl.osfragment.description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (name, version) = match (name, version) {
        (Some(n), Some(v)) => (n, v),
        _ => return None, // Missing required annotations — fall back to layer extraction
    };

    let repos: Vec<String> = annotations
        .get("com.github.marrusl.osfragment.provides.repos")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let required: Vec<String> = annotations
        .get("com.github.marrusl.osfragment.packages.required")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let vendor = annotations
        .get("com.github.marrusl.osfragment.vendor")
        .and_then(|v| v.as_str())
        .map(String::from);

    Some(Fragment {
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        vendor,
        provides: FragmentProvides { repos },
        packages: FragmentPackages { required },
        conflicts: FragmentConflicts { fragments: vec![] },
    })
}

/// Pull `image_ref` via skopeo into a temporary OCI layout and return the
/// raw bytes of each layer blob, in manifest order. Shared by every code
/// path that needs a fragment image's layer contents: the full metadata
/// load (`load_registry_fragment`) and self-contained materialization
/// (`materialize_fragment`).
fn pull_layer_bytes(image_ref: &str) -> Result<Vec<Vec<u8>>> {
    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let oci_path = tmp.path().join("oci-layout");

    let status = std::process::Command::new("skopeo")
        .args([
            "copy",
            "--override-os",
            "linux",
            &format!("docker://{}", image_ref),
            &format!("oci:{}", oci_path.display()),
        ])
        .status()
        .context("failed to run skopeo copy")?;

    if !status.success() {
        bail!("skopeo copy failed for {}", image_ref);
    }

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

    layers
        .iter()
        .map(|layer_desc| {
            let layer_digest = layer_desc["digest"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("no digest in layer descriptor"))?;
            let layer_blob_path = oci_path.join("blobs").join(layer_digest.replace(':', "/"));
            std::fs::read(&layer_blob_path)
                .with_context(|| format!("reading layer blob {}", layer_digest))
        })
        .collect()
}

pub fn load_registry_fragment(image_ref: &str) -> Result<LoadedFragment> {
    let digest = resolve_digest(image_ref)?;
    let (name, _tag) = split_image_ref(image_ref);
    let image_with_digest = format!("{}@{}", name, digest);

    // Assembly always parses the in-layer fragment.toml for the authoritative
    // Fragment.  The annotation fast path is limited to metadata-only
    // operations (inspect/list via load_registry_fragment_metadata_only)
    // because annotations omit fields like conflicts.
    let layer_bytes_list = pull_layer_bytes(&image_with_digest)?;

    // Scan all layers and aggregate: fragment.toml, tree paths, hooks,
    // and repo file contents may be spread across multiple layers.
    let mut fragment = None;
    let mut all_tree_paths = Vec::new();
    let mut all_hook_paths = Vec::new();
    let mut repo_file_contents = std::collections::HashMap::new();

    for layer_bytes in &layer_bytes_list {
        if fragment.is_none() {
            if let Ok(toml_content) = extract_fragment_toml_from_bytes(layer_bytes) {
                fragment = Some(parse_fragment_toml(&toml_content)?);
            }
        }

        let tree_paths = extract_tree_paths_from_bytes(layer_bytes)?;

        // Collect all executable files in hooks/ directory
        let hook_paths: Vec<PathBuf> = tree_paths
            .iter()
            .filter(|p| p.to_string_lossy().starts_with("fragment/hooks/"))
            .filter_map(|p| p.strip_prefix("fragment/hooks").ok())
            .map(|p| p.to_path_buf())
            .collect();
        all_hook_paths.extend(hook_paths);

        let remapped: Vec<PathBuf> = tree_paths
            .iter()
            .filter_map(|p| p.strip_prefix("fragment").ok())
            .map(|p| p.to_path_buf())
            .collect();
        all_tree_paths.extend(remapped);

        let layer_repo_contents = extract_repo_file_contents_from_bytes(layer_bytes)?;
        repo_file_contents.extend(layer_repo_contents);
    }

    let fragment = fragment.ok_or_else(|| {
        anyhow::anyhow!("no layer containing fragment/fragment.toml found in image")
    })?;
    let relative_paths = all_tree_paths;

    // Sort hooks alphabetically
    all_hook_paths.sort();

    Ok(LoadedFragment {
        fragment,
        tree_paths: relative_paths,
        hook_paths: all_hook_paths,
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
        // tree_paths and hook_paths are unknown in this path;
        // inspect/list can display fragment metadata without them.
        return Ok(LoadedFragment {
            fragment,
            tree_paths: vec![],
            hook_paths: vec![],
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
        assert!(!is_repo_path(Path::new("hooks/configure.sh")));
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

    /// A tar entry with an explicit mode and entry type, for tests that
    /// need directory entries or non-default permission bits —
    /// `create_test_tarball` above always writes regular files at 0o644.
    struct RawEntry<'a> {
        path: &'a str,
        data: &'a [u8],
        mode: u32,
        entry_type: tar::EntryType,
    }

    fn create_test_tarball_with_modes(entries: &[RawEntry]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for entry in entries {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(entry.entry_type);
                if header.set_path(entry.path).is_err() {
                    let name = &mut header.as_old_mut().name;
                    name.fill(0);
                    let path_bytes = entry.path.as_bytes();
                    let len = path_bytes.len().min(name.len());
                    name[..len].copy_from_slice(&path_bytes[..len]);
                }
                header.set_size(entry.data.len() as u64);
                header.set_mode(entry.mode);
                header.set_cksum();
                tar.append(&header, entry.data).unwrap();
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
                b"[fragment]\nname=\"x\"\nversion=\"1\"\ndescription=\"x\"",
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
                b"[fragment]\nname=\"a\"\nversion=\"1\"\ndescription=\"a\"",
            ),
            (
                "fragment/fragment.toml",
                b"[fragment]\nname=\"b\"\nversion=\"2\"\ndescription=\"b\"",
            ),
        ]);
        let result = extract_fragment_toml_from_bytes(&tarball);
        assert!(result.is_err());
    }

    #[test]
    fn validate_tar_entry_security_guards() {
        use tar::EntryType;
        let cases = [
            ("fragment/fragment.toml", EntryType::Regular, true, ""),
            ("fragment/tree/etc/foo.conf", EntryType::Regular, true, ""),
            // Absolute path inside /fragment/ is allowed
            ("/fragment/tree/etc/foo.conf", EntryType::Regular, true, ""),
            // Path traversal
            ("../etc/passwd", EntryType::Regular, false, "traversal"),
            (
                "fragment/../../../etc/shadow",
                EntryType::Regular,
                false,
                "traversal",
            ),
            // Absolute paths outside /fragment/
            ("/etc/passwd", EntryType::Regular, false, "absolute path"),
            ("/usr/bin/evil", EntryType::Regular, false, "absolute path"),
            // Symlinks and hardlinks
            (
                "fragment/tree/link",
                EntryType::Symlink,
                false,
                "symlink or hardlink",
            ),
            (
                "fragment/tree/link",
                EntryType::Link,
                false,
                "symlink or hardlink",
            ),
        ];
        for (path, entry_type, should_pass, expected_err) in &cases {
            let result = validate_tar_entry(path, *entry_type);
            if *should_pass {
                assert!(
                    result.is_ok(),
                    "expected pass for path '{}': {:?}",
                    path,
                    result
                );
            } else {
                let err = result.unwrap_err();
                assert!(
                    err.to_string().contains(expected_err),
                    "path '{}': expected error containing '{}', got '{}'",
                    path,
                    expected_err,
                    err
                );
            }
        }
    }

    #[test]
    fn extract_repo_contents_from_valid_layer() {
        let repo_content = b"[epel]\nname=EPEL\nbaseurl=https://example.com/epel/\n";
        let tarball =
            create_test_tarball(&[("fragment/tree/etc/yum.repos.d/epel.repo", repo_content)]);
        let result = extract_repo_file_contents_from_bytes(&tarball).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("epel.repo"));
        assert!(result["epel.repo"].contains("[epel]"));
    }

    #[test]
    fn hooks_collected_regardless_of_extension() {
        let tarball = create_test_tarball(&[
            ("fragment/hooks/01-setup.sh", b"#!/bin/bash\necho setup"),
            ("fragment/hooks/02-config.bash", b"#!/bin/bash\necho config"),
            ("fragment/hooks/configure", b"#!/bin/sh\necho configure"),
            (
                "fragment/hooks/setup.py",
                b"#!/usr/bin/env python3\nprint('ok')",
            ),
            ("fragment/tree/etc/foo.conf", b"data"),
        ]);
        let paths = extract_tree_paths_from_bytes(&tarball).unwrap();
        let hook_paths: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.to_string_lossy().starts_with("fragment/hooks/"))
            .filter_map(|p| p.strip_prefix("fragment/hooks").ok())
            .map(|p| p.to_path_buf())
            .collect();
        assert_eq!(hook_paths.len(), 4);
        let names: Vec<String> = hook_paths.iter().map(|p| p.display().to_string()).collect();
        assert!(names.contains(&"01-setup.sh".to_string()));
        assert!(names.contains(&"02-config.bash".to_string()));
        assert!(names.contains(&"configure".to_string()));
        assert!(names.contains(&"setup.py".to_string()));
    }

    #[test]
    fn payload_extracted_to_disk_matches_source_bytes() {
        let tree_content = b"[epel]\nname=EPEL\nbaseurl=https://example.com/epel/\n";
        let hook_content = b"#!/bin/sh\necho configure\n";
        let tarball = create_test_tarball(&[
            ("fragment/tree/etc/yum.repos.d/epel.repo", tree_content),
            ("fragment/hooks/configure.sh", hook_content),
        ]);

        let workdir = tempfile::tempdir().unwrap();
        extract_fragment_payload_to_disk(&tarball, workdir.path()).unwrap();

        let extracted_tree =
            std::fs::read(workdir.path().join("tree/etc/yum.repos.d/epel.repo")).unwrap();
        let extracted_hook = std::fs::read(workdir.path().join("hooks/configure.sh")).unwrap();
        assert_eq!(extracted_tree, tree_content);
        assert_eq!(extracted_hook, hook_content);
    }

    #[test]
    fn payload_extraction_rejects_traversal_like_other_extractors() {
        let tarball = create_test_tarball(&[
            ("../etc/passwd", b"evil"),
            ("fragment/tree/etc/foo.conf", b"data"),
        ]);
        let workdir = tempfile::tempdir().unwrap();
        let result = extract_fragment_payload_to_disk(&tarball, workdir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("traversal"));
    }

    #[test]
    fn empty_tree_directory_and_directory_mode_survive_extraction() {
        let tarball = create_test_tarball_with_modes(&[RawEntry {
            path: "fragment/tree/etc/empty-dir",
            data: b"",
            mode: 0o700,
            entry_type: tar::EntryType::Directory,
        }]);

        let workdir = tempfile::tempdir().unwrap();
        extract_fragment_payload_to_disk(&tarball, workdir.path()).unwrap();

        let dir_path = workdir.path().join("tree/etc/empty-dir");
        let metadata = std::fs::metadata(&dir_path).unwrap();
        assert!(
            metadata.is_dir(),
            "empty directory did not survive extraction"
        );

        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o700,
            "directory mode was not preserved"
        );
    }

    #[test]
    fn annotations_populate_fragment_from_project_namespace() {
        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.name": "tailscale",
            "com.github.marrusl.osfragment.version": "1.82.0",
            "com.github.marrusl.osfragment.description": "Tailscale mesh VPN",
            "com.github.marrusl.osfragment.vendor": "Tailscale Inc.",
            "com.github.marrusl.osfragment.provides.repos": "[\"tailscale\"]",
            "com.github.marrusl.osfragment.packages.required": "[\"tailscale\"]",
        });

        let frag = fragment_from_annotations(&annotations).expect("all required keys present");
        assert_eq!(frag.name, "tailscale");
        assert_eq!(frag.version, "1.82.0");
        assert_eq!(frag.description, "Tailscale mesh VPN");
        assert_eq!(frag.vendor.as_deref(), Some("Tailscale Inc."));
        assert_eq!(frag.provides.repos, vec!["tailscale"]);
        assert_eq!(frag.packages.required, vec!["tailscale"]);
    }

    /// A fragment published while `phase` was still emitted carries a seventh
    /// annotation. Lookups are per-key, so the extra one is ignored and the
    /// fast path still resolves: already-published fragments keep working.
    #[test]
    fn stale_phase_annotation_is_ignored_by_fast_path() {
        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.name": "epel",
            "com.github.marrusl.osfragment.version": "10",
            "com.github.marrusl.osfragment.phase": "repos",
        });

        let frag = fragment_from_annotations(&annotations)
            .expect("stale phase key must not block resolution");
        assert_eq!(frag.name, "epel");
    }

    #[test]
    fn retired_bootc_annotations_do_not_satisfy_fast_path() {
        let annotations = serde_json::json!({
            "io.bootc.fragment.name": "tailscale",
            "io.bootc.fragment.version": "1.82.0",
            "io.bootc.fragment.phase": "config",
        });

        assert!(
            fragment_from_annotations(&annotations).is_none(),
            "old-namespace keys must not satisfy the fast path; the fragment falls back to layer extraction"
        );
    }
}
