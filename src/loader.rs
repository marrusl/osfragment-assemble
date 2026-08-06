use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::path::{Path, PathBuf};

use crate::fragment::{
    is_repo_path, parse_fragment_toml, Fragment, FragmentConflicts, FragmentName, FragmentPackages,
    FragmentProvides,
};
use crate::generator::split_image_ref;
use crate::manifest::{FragmentSource, Manifest};

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

/// The single file under a fragment's `hooks/` that the tool runs.
pub const HOOKS_ENTRYPOINT_NAME: &str = "entrypoint";

/// The same file as it appears inside a fragment layer.
const HOOKS_ENTRYPOINT_TAR_PATH: &str = "fragment/hooks/entrypoint";

/// Execute bits (`u+x`, `g+x`, `o+x`). Root's `CAP_DAC_OVERRIDE` grants
/// execute only when at least one of them is set, so this mask matches
/// exactly what is runnable at build time.
const EXECUTE_BITS: u32 = 0o111;

/// Enforce the hooks entrypoint contract for a fragment that carries at least
/// one hook file: `hooks/entrypoint` must exist as an executable regular file,
/// and it is the only thing the tool runs.
///
/// `entrypoint_mode` is the mode of `hooks/entrypoint` when it is a regular
/// file, and `None` when it is missing or is something else (a directory of
/// that name is not an entrypoint). Both validation sites — the registry load,
/// reading the mode off the tar header, and `inspect`, reading it off the
/// filesystem — call this so their messages cannot drift apart.
pub fn validate_hooks_entrypoint(fragment_name: &str, entrypoint_mode: Option<u32>) -> Result<()> {
    match entrypoint_mode {
        None => bail!(
            "fragment '{}': hooks/ contains files but no executable hooks/entrypoint; \
             the entrypoint is the single file osfragment-assemble runs. Rename the \
             script to hooks/entrypoint, or add one that invokes the others.",
            fragment_name
        ),
        Some(mode) if mode & EXECUTE_BITS == 0 => bail!(
            "fragment '{}': hooks/entrypoint is not executable; the entrypoint is the \
             single file osfragment-assemble runs. Set the execute bit (chmod +x) before \
             building the fragment image.",
            fragment_name
        ),
        Some(_) => Ok(()),
    }
}

/// Shared validation for tar entries across all extraction functions.
/// Rejects path traversal, absolute paths outside /fragment/, non-UTF-8
/// entry names, and symlinks/hardlinks.
///
/// Returns the entry's path in the one canonical form every matcher in this
/// module compares against: relative, with a leading `/` and any `.`
/// component removed. Tar archives legitimately carry the same member as
/// `fragment/hooks/entrypoint`, `./fragment/hooks/entrypoint`, or
/// `/fragment/hooks/entrypoint` depending on the builder that produced the
/// layer, and matching the raw path saw only the first: a fragment whose
/// hooks arrived in either other form had them silently dropped from
/// detection, which skipped the entrypoint check while the files still
/// landed in the built image.
///
/// The returned path is a faithful rendering of the entry name, not merely
/// a plausible one, which is why a non-UTF-8 name is rejected rather than
/// converted lossily. `extract_fragment_payload_to_disk` derives the file
/// it writes from this value: under a lossy conversion two entries
/// differing only in invalid bytes would land on one destination, last
/// write winning, and every other such name would materialize with
/// replacement characters. Rejecting matches how this function already
/// treats `..` and absolute paths, and keeps what the fragment author wrote
/// the same as what lands on disk.
///
/// The checks run on the raw path, before normalization, so stripping the
/// leading `/` here cannot defeat the absolute-path check above it.
/// Returning the normalized path rather than `()` is what keeps callers
/// from reaching for the raw one by accident.
fn validate_tar_entry(path: &Path, entry_type: tar::EntryType) -> Result<PathBuf> {
    let path_str = path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "non-UTF-8 entry name rejected in fragment layer: {}",
            path.to_string_lossy().escape_debug()
        )
    })?;
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
    // `..` is already rejected above, so keeping only `Normal` components
    // drops exactly the leading `.` that `Components` preserves.
    Ok(Path::new(path_str.trim_start_matches('/'))
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .collect())
}

pub fn extract_fragment_toml_from_bytes(compressed: &[u8]) -> Result<String> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);

    let mut found: Option<String> = None;

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let path = entry.path().context("reading entry path")?.to_path_buf();
        let path = validate_tar_entry(&path, entry.header().entry_type())?;

        if path == Path::new(FRAGMENT_TOML_PATH) {
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

/// Regular-file paths in a layer, plus the mode of
/// `fragment/hooks/entrypoint` when this layer carries one as a regular file.
///
/// The mode comes off the header this loop already holds, so enforcing the
/// entrypoint contract costs no second pass over the archive.
fn extract_tree_paths_from_bytes(compressed: &[u8]) -> Result<(Vec<PathBuf>, Option<u32>)> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let mut paths = Vec::new();
    let mut entrypoint_mode = None;

    for entry_result in archive.entries()? {
        let entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        let path = validate_tar_entry(&path, entry.header().entry_type())?;
        if entry.header().entry_type().is_file() {
            if path == Path::new(HOOKS_ENTRYPOINT_TAR_PATH) {
                entrypoint_mode = Some(entry.header().mode()?);
            }
            paths.push(path);
        }
    }
    Ok((paths, entrypoint_mode))
}

fn extract_repo_file_contents_from_bytes(
    compressed: &[u8],
) -> Result<std::collections::HashMap<String, String>> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let mut contents = std::collections::HashMap::new();

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        let path = validate_tar_entry(&path, entry.header().entry_type())?;
        let path_str = path.to_string_lossy();

        if path_str.contains("yum.repos.d/") && path_str.ends_with(".repo") {
            let filename = path.file_name().map(|f| f.to_string_lossy().to_string());
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
        let path = validate_tar_entry(&path, entry.header().entry_type())?;

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
/// Returns `None` when a required key is missing or when the annotated name
/// does not satisfy the fragment-name grammar; the caller falls back to
/// layer extraction. Falling back rather than failing is deliberate: the
/// annotations are a cache of the in-layer `fragment.toml`, which is
/// authoritative, so a bad annotation costs a layer pull and then either
/// resolves against the real name or is rejected there.
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
        name: FragmentName::new(name).ok()?,
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

/// A fragment's metadata as aggregated from its layers, before the
/// registry-specific fields (source, digest, manifest index) are attached.
#[derive(Debug)]
struct LayeredMetadata {
    fragment: Fragment,
    tree_paths: Vec<PathBuf>,
    hook_paths: Vec<PathBuf>,
    repo_file_contents: std::collections::HashMap<String, String>,
}

/// Scan all layers and aggregate: fragment.toml, tree paths, hooks, and repo
/// file contents may be spread across multiple layers. The hooks entrypoint
/// contract is enforced here, once, against the aggregate.
///
/// Split out of `load_registry_fragment` so the aggregation — including that
/// contract — is exercisable from fixture layers without a registry.
fn fragment_from_layers(layer_bytes_list: &[Vec<u8>]) -> Result<LayeredMetadata> {
    let mut fragment = None;
    let mut all_tree_paths = Vec::new();
    let mut all_hook_paths = Vec::new();
    let mut entrypoint_mode = None;
    let mut repo_file_contents = std::collections::HashMap::new();

    for layer_bytes in layer_bytes_list {
        if fragment.is_none() {
            if let Ok(toml_content) = extract_fragment_toml_from_bytes(layer_bytes) {
                fragment = Some(parse_fragment_toml(&toml_content)?);
            }
        }

        let (tree_paths, layer_entrypoint_mode) = extract_tree_paths_from_bytes(layer_bytes)?;
        // Later layers shadow earlier ones, so the last entrypoint wins.
        if layer_entrypoint_mode.is_some() {
            entrypoint_mode = layer_entrypoint_mode;
        }

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

    // Sort hooks alphabetically
    all_hook_paths.sort();

    if !all_hook_paths.is_empty() {
        validate_hooks_entrypoint(fragment.name.as_str(), entrypoint_mode)?;
    }

    Ok(LayeredMetadata {
        fragment,
        tree_paths: all_tree_paths,
        hook_paths: all_hook_paths,
        repo_file_contents,
    })
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
    let metadata = fragment_from_layers(&layer_bytes_list)?;

    Ok(LoadedFragment {
        fragment: metadata.fragment,
        tree_paths: metadata.tree_paths,
        hook_paths: metadata.hook_paths,
        source: FragmentSource::Registry {
            image_ref: image_with_digest,
        },
        resolved_digest: Some(digest),
        manifest_index: 0, // set by caller
        repo_file_contents: metadata.repo_file_contents,
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

/// Whether fragment digests (and the digest-pinned `FragmentSource`) should
/// survive [`load_all_fragments`] for use downstream.
///
/// `--pin-digests` keeps them for default mode's named-stage emission and
/// digest comments. `--self-contained` also needs them kept, independently
/// of `--pin-digests`: materialization must pull exactly the digest that was
/// validated, even though the emitted Containerfile never exposes that digest
/// (see generator.rs's self-contained suppression).
pub fn should_keep_fragment_digests(pin_digests: bool, self_contained: Option<&Path>) -> bool {
    pin_digests || self_contained.is_some()
}

/// Load every fragment the manifest names, in manifest order.
///
/// `keep_digests`: whether to leave each fragment's digest-pinned
/// `FragmentSource`/`resolved_digest` in place. See
/// [`should_keep_fragment_digests`] for why this isn't simply `pin_digests`.
pub fn load_all_fragments(manifest: &Manifest, keep_digests: bool) -> Result<Vec<LoadedFragment>> {
    load_all_fragments_with(manifest, keep_digests, load_registry_fragment)
}

/// [`load_all_fragments`] with the per-fragment registry load injected.
///
/// Everything this function decides (manifest ordering, `manifest_index`
/// assignment, digest stripping, and which errors abort the run) is
/// registry-independent, but the real loader shells out to skopeo for every
/// fragment, so none of it was reachable from a test while the two were
/// welded together. Splitting them follows the
/// `write_output`/`write_output_with` precedent in `self_contained.rs`.
fn load_all_fragments_with(
    manifest: &Manifest,
    keep_digests: bool,
    load: impl Fn(&str) -> Result<LoadedFragment>,
) -> Result<Vec<LoadedFragment>> {
    let mut fragments = Vec::new();
    let total = manifest.fragments.len();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = mf.resolve_source()?;
        let FragmentSource::Registry { ref image_ref } = source;
        eprintln!("Loading fragment {}/{}: {}...", idx + 1, total, image_ref);
        let mut loaded = load(image_ref)?;
        if !keep_digests {
            // Use the manifest's declared image ref, not the digest-pinned ref
            loaded.source = FragmentSource::Registry {
                image_ref: image_ref.clone(),
            };
            loaded.resolved_digest = None;
        }
        eprintln!(
            "  {} ({})",
            loaded.fragment.name, loaded.fragment.description
        );
        loaded.manifest_index = idx;
        fragments.push(loaded);
    }

    // No reordering: emission follows manifest order, which is user intent.
    Ok(fragments)
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

    /// Digest a stub load reports, standing in for what skopeo would resolve.
    const TEST_DIGEST: &str = "sha256:0123456789abcdef";

    fn test_manifest(images: &[&str]) -> Manifest {
        use crate::manifest::ManifestFragment;
        Manifest {
            base: "quay.io/test/base:1".into(),
            fragments: images
                .iter()
                .map(|image| ManifestFragment {
                    image: (*image).into(),
                    packages: vec![],
                    mirror: None,
                })
                .collect(),
            source_path: "test-manifest.yaml".into(),
        }
    }

    /// Stands in for `load_registry_fragment`, returning what a real registry
    /// load returns: a digest-pinned source and a resolved digest, so the
    /// stripping branch has something to strip.
    fn stub_loaded(image_ref: &str) -> LoadedFragment {
        use crate::fragment::{
            Fragment, FragmentConflicts, FragmentName, FragmentPackages, FragmentProvides,
        };
        let (name, _tag) = split_image_ref(image_ref);
        let short = name.rsplit('/').next().unwrap_or("frag");
        LoadedFragment {
            fragment: Fragment {
                name: FragmentName::new(short).expect("test fragment name must be valid"),
                version: "1".into(),
                description: "test".into(),
                vendor: None,
                provides: FragmentProvides { repos: vec![] },
                packages: FragmentPackages { required: vec![] },
                conflicts: FragmentConflicts { fragments: vec![] },
            },
            tree_paths: vec![],
            hook_paths: vec![],
            source: FragmentSource::Registry {
                image_ref: format!("{}@{}", name, TEST_DIGEST),
            },
            resolved_digest: Some(TEST_DIGEST.into()),
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn keep_fragment_digests_cases() {
        // (pin_digests, self_contained, expected)
        let cases = [
            (false, None, false),
            (false, Some(Path::new("out")), true),
            (true, None, true),
            (true, Some(Path::new("out")), true),
        ];
        for (pin_digests, self_contained, expected) in cases {
            assert_eq!(
                should_keep_fragment_digests(pin_digests, self_contained),
                expected,
                "pin_digests={pin_digests}, self_contained={self_contained:?}"
            );
        }
    }

    #[test]
    fn load_all_fragments_digest_retention_tracks_keep_digests() {
        // (keep_digests, digest survives the load)
        let cases = [(true, true), (false, false)];
        for (keep_digests, survives) in cases {
            let manifest = test_manifest(&["quay.io/test/epel:10"]);
            let loaded = load_all_fragments_with(&manifest, keep_digests, |r| Ok(stub_loaded(r)))
                .expect("stub load cannot fail");
            let FragmentSource::Registry { image_ref } = &loaded[0].source;

            assert_eq!(
                loaded[0].resolved_digest.is_some(),
                survives,
                "resolved_digest must track keep_digests={keep_digests}"
            );
            assert_eq!(
                image_ref.contains('@'),
                survives,
                "source ref pinning must track keep_digests={keep_digests}"
            );
            if !survives {
                assert_eq!(
                    image_ref, "quay.io/test/epel:10",
                    "stripping restores the manifest's declared ref, tag included"
                );
            }
        }
    }

    #[test]
    fn load_all_fragments_indexes_and_orders_by_manifest_position() {
        let manifest = test_manifest(&[
            "quay.io/test/epel:10",
            "quay.io/test/nginx:1",
            "quay.io/test/grafana:11",
        ]);
        let loaded = load_all_fragments_with(&manifest, true, |r| Ok(stub_loaded(r)))
            .expect("stub load cannot fail");

        let indices: Vec<usize> = loaded.iter().map(|f| f.manifest_index).collect();
        assert_eq!(
            indices,
            vec![0, 1, 2],
            "manifest_index is the slice position"
        );

        let names: Vec<String> = loaded.iter().map(|f| f.fragment.name.to_string()).collect();
        assert_eq!(
            names,
            vec!["epel", "nginx", "grafana"],
            "emission order is manifest order, never sorted or grouped"
        );
    }

    #[test]
    fn load_all_fragments_on_empty_manifest_loads_nothing() {
        let manifest = test_manifest(&[]);
        let calls = std::cell::Cell::new(0);
        let loaded = load_all_fragments_with(&manifest, true, |r| {
            calls.set(calls.get() + 1);
            Ok(stub_loaded(r))
        })
        .expect("an empty manifest is not an error");

        assert!(loaded.is_empty());
        assert_eq!(
            calls.get(),
            0,
            "an empty manifest must not reach a registry"
        );
    }

    #[test]
    fn load_all_fragments_rejects_dir_source_before_loading() {
        let manifest = test_manifest(&["dir:./local-fragment"]);
        let calls = std::cell::Cell::new(0);
        let err = load_all_fragments_with(&manifest, true, |r| {
            calls.set(calls.get() + 1);
            Ok(stub_loaded(r))
        })
        .expect_err("dir: sources are unsupported");

        assert!(err.to_string().contains("dir:"), "got: {err}");
        assert_eq!(
            calls.get(),
            0,
            "a dir: source must fail before any load is attempted"
        );
    }

    #[test]
    fn load_all_fragments_aborts_at_the_first_failing_fragment() {
        let manifest = test_manifest(&[
            "quay.io/test/epel:10",
            "quay.io/test/broken:1",
            "quay.io/test/nginx:1",
        ]);
        let calls = std::cell::Cell::new(0);
        let err = load_all_fragments_with(&manifest, true, |r| {
            calls.set(calls.get() + 1);
            if r.contains("broken") {
                bail!("simulated registry failure");
            }
            Ok(stub_loaded(r))
        })
        .expect_err("a failing fragment fails the run");

        assert!(err.to_string().contains("simulated registry failure"));
        assert_eq!(
            calls.get(),
            2,
            "the run stops at the failing fragment rather than continuing to the third"
        );
    }
}

#[cfg(test)]
mod layer_tests {
    use super::*;

    /// Regular files at 0o644, the default every path-only fixture wants.
    /// Delegates so there is exactly one fixture builder, and therefore
    /// exactly one path-transparency guarantee, in this module.
    fn create_test_tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let raw: Vec<RawEntry> = entries
            .iter()
            .map(|&(path, data)| RawEntry {
                path: path.as_bytes(),
                data,
                mode: 0o644,
                entry_type: tar::EntryType::Regular,
            })
            .collect();
        create_test_tarball_with_modes(&raw)
    }

    /// A tar entry with an explicit mode and entry type.
    ///
    /// `path` is bytes rather than `&str` because a tar member name is
    /// arbitrary bytes on Unix, and the rejection of non-UTF-8 names is
    /// only testable by a fixture that can write one.
    struct RawEntry<'a> {
        path: &'a [u8],
        data: &'a [u8],
        mode: u32,
        entry_type: tar::EntryType,
    }

    /// The old-style ustar header's name field. A raw write past this length
    /// is truncated with no error from the header, so the builder below
    /// refuses rather than letting a test assert about a truncated path.
    const USTAR_NAME_FIELD_BYTES: usize = 100;

    /// Writes each entry's path verbatim into the raw header name field
    /// rather than going through `tar::Header::set_path`. `set_path` is not
    /// path-transparent: it normalizes a leading `./` away and rejects both
    /// `..` components and absolute paths. Real layers carry all three forms,
    /// so a fixture built through `set_path` cannot express them, and a test
    /// written against one silently asserts about the normalized path instead
    /// of the one it names.
    fn create_test_tarball_with_modes(entries: &[RawEntry]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for entry in entries {
                assert!(
                    entry.path.len() <= USTAR_NAME_FIELD_BYTES,
                    "fixture path {:?} is {} bytes; the ustar header name field holds {}. \
                     A longer path is silently truncated by this raw write, so the test \
                     would assert about the truncated path rather than the one it names.",
                    String::from_utf8_lossy(entry.path),
                    entry.path.len(),
                    USTAR_NAME_FIELD_BYTES
                );
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(entry.entry_type);
                let name = &mut header.as_old_mut().name;
                name.fill(0);
                name[..entry.path.len()].copy_from_slice(entry.path);
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
            let result = validate_tar_entry(Path::new(path), *entry_type);
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

    /// The canonical form `validate_tar_entry` hands every matcher. Pinned
    /// directly, so a change to the normalization shows up here rather than
    /// only as a downstream detection failure.
    #[test]
    fn validate_tar_entry_returns_a_canonical_relative_path() {
        let cases = [
            ("fragment/hooks/entrypoint", "fragment/hooks/entrypoint"),
            ("./fragment/hooks/entrypoint", "fragment/hooks/entrypoint"),
            ("/fragment/hooks/entrypoint", "fragment/hooks/entrypoint"),
            ("./fragment/tree/etc/app.conf", "fragment/tree/etc/app.conf"),
            ("/fragment/tree/", "fragment/tree"),
            ("fragment/./hooks/entrypoint", "fragment/hooks/entrypoint"),
        ];
        for (raw, expected) in cases {
            let normalized = validate_tar_entry(Path::new(raw), tar::EntryType::Regular)
                .unwrap_or_else(|e| panic!("{raw} must validate: {e}"));
            assert_eq!(
                normalized,
                PathBuf::from(expected),
                "{raw} normalized wrong"
            );
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
        let (paths, _entrypoint_mode) = extract_tree_paths_from_bytes(&tarball).unwrap();
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

    /// A tar member name is arbitrary bytes on Unix. Deriving the write
    /// destination from a lossy UTF-8 conversion collapsed two distinct
    /// entries onto one path, last write winning, and materialized any other
    /// non-UTF-8 name with replacement characters. Both are silent. Rejecting
    /// keeps the guarantee that what the fragment author wrote is what lands
    /// on disk.
    #[test]
    fn non_utf8_entry_names_are_rejected_rather_than_materialized_lossily() {
        // Two distinct names differing only in bytes that are not valid
        // UTF-8. A lossy conversion maps both to `.../a<U+FFFD>.conf`.
        let tarball = create_test_tarball_with_modes(&[
            RawEntry {
                path: b"fragment/tree/etc/a\xff.conf",
                data: b"first",
                mode: 0o644,
                entry_type: tar::EntryType::Regular,
            },
            RawEntry {
                path: b"fragment/tree/etc/a\xfe.conf",
                data: b"second",
                mode: 0o644,
                entry_type: tar::EntryType::Regular,
            },
        ]);

        let workdir = tempfile::tempdir().unwrap();
        let err = extract_fragment_payload_to_disk(&tarball, workdir.path())
            .expect_err("a non-UTF-8 entry name must be rejected, not mangled onto disk");
        assert!(
            err.to_string().contains("non-UTF-8"),
            "wrong error for a non-UTF-8 entry name: {err}"
        );
    }

    #[test]
    fn empty_tree_directory_and_directory_mode_survive_extraction() {
        let tarball = create_test_tarball_with_modes(&[RawEntry {
            path: b"fragment/tree/etc/empty-dir",
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

    const TEST_FRAGMENT_TOML: &[u8] = br#"[fragment]
name = "nvidia-driver"
version = "1.0"
description = "test fragment"
"#;

    /// A one-layer fragment carrying `fragment.toml` plus the given entries,
    /// for driving `fragment_from_layers` without a registry.
    fn fragment_layers(entries: Vec<RawEntry>) -> Vec<Vec<u8>> {
        let mut all = vec![RawEntry {
            path: FRAGMENT_TOML_PATH.as_bytes(),
            data: TEST_FRAGMENT_TOML,
            mode: 0o644,
            entry_type: tar::EntryType::Regular,
        }];
        all.extend(entries);
        vec![create_test_tarball_with_modes(&all)]
    }

    fn hook_entry<'a>(path: &'a str, mode: u32) -> RawEntry<'a> {
        RawEntry {
            path: path.as_bytes(),
            data: b"#!/bin/sh\necho hook\n",
            mode,
            entry_type: tar::EntryType::Regular,
        }
    }

    /// These two messages are the contract's whole user interface — a fragment
    /// author reads one line of stderr, not this function — and the spec fixes
    /// their wording. The substring assertions below would not catch a reflow,
    /// a dropped remediation sentence, or a space lost at a `\` continuation;
    /// that last one is invisible at review time because it sits at
    /// end-of-line. Single-line by design: the terminal wraps at its own width,
    /// and no other error in this crate embeds a newline.
    ///
    /// The expected strings are deliberately unbroken source lines, so this
    /// test cannot suffer the continuation fault it exists to detect.
    #[test]
    fn entrypoint_errors_are_verbatim_single_lines() {
        assert_eq!(
            validate_hooks_entrypoint("nvidia-driver", None)
                .unwrap_err()
                .to_string(),
            "fragment 'nvidia-driver': hooks/ contains files but no executable hooks/entrypoint; the entrypoint is the single file osfragment-assemble runs. Rename the script to hooks/entrypoint, or add one that invokes the others."
        );
        assert_eq!(
            validate_hooks_entrypoint("nvidia-driver", Some(0o644))
                .unwrap_err()
                .to_string(),
            "fragment 'nvidia-driver': hooks/entrypoint is not executable; the entrypoint is the single file osfragment-assemble runs. Set the execute bit (chmod +x) before building the fragment image."
        );
    }

    #[test]
    fn hooks_without_entrypoint_are_rejected() {
        let layers = fragment_layers(vec![hook_entry("fragment/hooks/other.sh", 0o755)]);
        let err = fragment_from_layers(&layers).unwrap_err().to_string();
        assert!(
            err.contains("nvidia-driver") && err.contains("no executable hooks/entrypoint"),
            "error must name the fragment and hooks/entrypoint, got: {err}"
        );
    }

    #[test]
    fn non_executable_entrypoint_is_rejected() {
        let layers = fragment_layers(vec![hook_entry("fragment/hooks/entrypoint", 0o644)]);
        let err = fragment_from_layers(&layers).unwrap_err().to_string();
        assert!(
            err.contains("nvidia-driver")
                && err.contains("hooks/entrypoint is not executable")
                && err.contains("chmod +x"),
            "error must name the mode problem and its fix, got: {err}"
        );
    }

    #[test]
    fn valid_hook_shapes_are_accepted() {
        let cases: Vec<(&str, Vec<RawEntry>)> = vec![
            (
                "zero hook files",
                vec![RawEntry {
                    path: b"fragment/tree/etc/foo.conf",
                    data: b"data",
                    mode: 0o644,
                    entry_type: tar::EntryType::Regular,
                }],
            ),
            (
                "entrypoint alone",
                vec![hook_entry("fragment/hooks/entrypoint", 0o755)],
            ),
            (
                "entrypoint alongside other files",
                vec![
                    hook_entry("fragment/hooks/entrypoint", 0o755),
                    hook_entry("fragment/hooks/nvidia.run", 0o755),
                    hook_entry("fragment/hooks/README", 0o644),
                ],
            ),
            (
                "entrypoint alongside a subdirectory",
                vec![
                    hook_entry("fragment/hooks/entrypoint", 0o755),
                    RawEntry {
                        path: b"fragment/hooks/lib",
                        data: b"",
                        mode: 0o755,
                        entry_type: tar::EntryType::Directory,
                    },
                    hook_entry("fragment/hooks/lib/helper.sh", 0o644),
                ],
            ),
            (
                "entrypoint executable by group only",
                vec![hook_entry("fragment/hooks/entrypoint", 0o610)],
            ),
        ];

        for (label, entries) in cases {
            let result = fragment_from_layers(&fragment_layers(entries));
            assert!(result.is_ok(), "{label} must load: {:?}", result.err());
        }
    }

    /// The hook list survives validation: `inspect` still displays every file
    /// under `hooks/`, including the ones the tool will never invoke.
    #[test]
    fn support_files_stay_in_the_hook_list() {
        let layers = fragment_layers(vec![
            hook_entry("fragment/hooks/entrypoint", 0o755),
            hook_entry("fragment/hooks/lib/helper.sh", 0o644),
        ]);
        let metadata = fragment_from_layers(&layers).unwrap();
        let names: Vec<String> = metadata
            .hook_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(names, vec!["entrypoint", "lib/helper.sh"]);
    }

    #[test]
    fn nested_entrypoint_does_not_satisfy_the_rule() {
        let layers = fragment_layers(vec![hook_entry("fragment/hooks/lib/entrypoint", 0o755)]);
        let err = fragment_from_layers(&layers).unwrap_err().to_string();
        assert!(
            err.contains("no executable hooks/entrypoint"),
            "only hooks/entrypoint counts, got: {err}"
        );
    }

    #[test]
    fn entrypoint_as_a_directory_is_rejected() {
        let layers = fragment_layers(vec![
            RawEntry {
                path: b"fragment/hooks/entrypoint",
                data: b"",
                mode: 0o755,
                entry_type: tar::EntryType::Directory,
            },
            hook_entry("fragment/hooks/entrypoint/real.sh", 0o755),
        ]);
        let err = fragment_from_layers(&layers).unwrap_err().to_string();
        assert!(
            err.contains("no executable hooks/entrypoint"),
            "a directory named entrypoint is not an entrypoint, got: {err}"
        );
    }

    /// The regression that would reintroduce auto-discovery: an old-style
    /// fragment whose hooks were chained by filename order must fail to load
    /// rather than run anything.
    #[test]
    fn old_style_chained_hooks_are_rejected() {
        let layers = fragment_layers(vec![
            hook_entry("fragment/hooks/01-setup.sh", 0o755),
            hook_entry("fragment/hooks/02-config.sh", 0o755),
        ]);
        let err = fragment_from_layers(&layers).unwrap_err().to_string();
        assert!(
            err.contains("no executable hooks/entrypoint"),
            "old-layout fragments must be rejected, not chained, got: {err}"
        );
    }

    /// The three forms a tar archive can carry the same member as. A layer
    /// built by one tool arrives unprefixed, another emits `./`, and
    /// `validate_tar_entry` has always permitted `/fragment/...` outright.
    const LAYER_PATH_PREFIXES: [&str; 3] = ["", "./", "/"];

    /// Hook detection must not depend on which of the three forms the layer
    /// uses. Before normalization, `./` and `/` prefixed hooks were dropped
    /// from `hook_paths` entirely, so the entrypoint contract was never
    /// evaluated: a fragment with a non-executable entrypoint (or none at
    /// all) loaded clean while its hooks still landed in the built image.
    #[test]
    fn hook_detection_is_prefix_independent() {
        for prefix in LAYER_PATH_PREFIXES {
            let entrypoint = format!("{prefix}fragment/hooks/entrypoint");
            let helper = format!("{prefix}fragment/hooks/lib/helper.sh");

            // A non-executable entrypoint must be rejected in every form.
            let layers = fragment_layers(vec![hook_entry(&entrypoint, 0o644)]);
            let err = fragment_from_layers(&layers)
                .expect_err(&format!("prefix {prefix:?}: must be rejected"))
                .to_string();
            assert!(
                err.contains("hooks/entrypoint is not executable"),
                "prefix {prefix:?}: wrong error: {err}"
            );

            // Hooks with no entrypoint at all must be rejected in every form.
            let layers = fragment_layers(vec![hook_entry(&helper, 0o644)]);
            let err = fragment_from_layers(&layers)
                .expect_err(&format!("prefix {prefix:?}: must be rejected"))
                .to_string();
            assert!(
                err.contains("no executable hooks/entrypoint"),
                "prefix {prefix:?}: wrong error: {err}"
            );

            // A well-formed fragment must load, with hooks listed under the
            // same canonical names regardless of the form they arrived in.
            let layers = fragment_layers(vec![
                hook_entry(&entrypoint, 0o755),
                hook_entry(&helper, 0o644),
            ]);
            let metadata = fragment_from_layers(&layers)
                .unwrap_or_else(|e| panic!("prefix {prefix:?}: must load: {e}"));
            let names: Vec<String> = metadata
                .hook_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            assert_eq!(
                names,
                vec!["entrypoint", "lib/helper.sh"],
                "prefix {prefix:?}: hook paths must be canonical"
            );
        }
    }

    /// `fragment.toml` discovery and tree-path collection go through the same
    /// matcher, so they carry the same prefix sensitivity. A `/`-prefixed
    /// layer previously failed with "no layer containing fragment/fragment.toml
    /// found in image" even though the file was right there.
    #[test]
    fn toml_and_tree_paths_are_prefix_independent() {
        for prefix in LAYER_PATH_PREFIXES {
            let toml_path = format!("{prefix}fragment/fragment.toml");
            let conf_path = format!("{prefix}fragment/tree/etc/app.conf");
            let repo_path = format!("{prefix}fragment/tree/etc/yum.repos.d/epel.repo");
            let layers = vec![create_test_tarball_with_modes(&[
                RawEntry {
                    path: toml_path.as_bytes(),
                    data: TEST_FRAGMENT_TOML,
                    mode: 0o644,
                    entry_type: tar::EntryType::Regular,
                },
                RawEntry {
                    path: conf_path.as_bytes(),
                    data: b"key=value\n",
                    mode: 0o644,
                    entry_type: tar::EntryType::Regular,
                },
                RawEntry {
                    path: repo_path.as_bytes(),
                    data: b"[epel]\nbaseurl=https://example.com/epel/\n",
                    mode: 0o644,
                    entry_type: tar::EntryType::Regular,
                },
            ])];

            let metadata = fragment_from_layers(&layers)
                .unwrap_or_else(|e| panic!("prefix {prefix:?}: must load: {e}"));
            assert_eq!(metadata.fragment.name, "nvidia-driver");

            let mut tree: Vec<String> = metadata
                .tree_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            tree.sort();
            // fragment.toml rides along in tree_paths (consumers filter on
            // the `tree/` prefix); the point here is that every entry is
            // remapped to the same canonical form whatever the layer used.
            assert_eq!(
                tree,
                vec![
                    "fragment.toml",
                    "tree/etc/app.conf",
                    "tree/etc/yum.repos.d/epel.repo"
                ],
                "prefix {prefix:?}: tree paths must be canonical"
            );
            assert!(
                metadata.repo_file_contents.contains_key("epel.repo"),
                "prefix {prefix:?}: repo file contents must be collected"
            );
        }
    }

    /// Materialization shares the matcher, so a prefixed layer previously
    /// wrote nothing to disk at all: every entry fell through the
    /// `strip_prefix` chain to `continue`.
    #[test]
    fn payload_extraction_is_prefix_independent() {
        for prefix in LAYER_PATH_PREFIXES {
            let conf_path = format!("{prefix}fragment/tree/etc/app.conf");
            let hook_path = format!("{prefix}fragment/hooks/entrypoint");
            let tarball = create_test_tarball_with_modes(&[
                RawEntry {
                    path: conf_path.as_bytes(),
                    data: b"key=value\n",
                    mode: 0o644,
                    entry_type: tar::EntryType::Regular,
                },
                RawEntry {
                    path: hook_path.as_bytes(),
                    data: b"#!/bin/sh\necho hook\n",
                    mode: 0o755,
                    entry_type: tar::EntryType::Regular,
                },
            ]);

            let workdir = tempfile::tempdir().unwrap();
            extract_fragment_payload_to_disk(&tarball, workdir.path()).unwrap();
            assert_eq!(
                std::fs::read(workdir.path().join("tree/etc/app.conf")).unwrap(),
                b"key=value\n",
                "prefix {prefix:?}: tree file must be materialized"
            );
            assert!(
                workdir.path().join("hooks/entrypoint").exists(),
                "prefix {prefix:?}: hook must be materialized"
            );
        }
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

    /// The annotation fast path is the second place a name enters the tool,
    /// and it bypasses `fragment.toml` entirely. A name that fails the
    /// grammar must not satisfy it: the caller then falls back to layer
    /// extraction, where the authoritative name is parsed and validated.
    #[test]
    fn path_unsafe_annotation_name_does_not_satisfy_fast_path() {
        for bad in ["../../escape", "a/b", "", "EPEL"] {
            let annotations = serde_json::json!({
                "com.github.marrusl.osfragment.name": bad,
                "com.github.marrusl.osfragment.version": "1.0",
            });
            assert!(
                fragment_from_annotations(&annotations).is_none(),
                "annotated name '{bad}' must not satisfy the fast path"
            );
        }
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
