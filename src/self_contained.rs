//! Self-contained output mode: materializes fragment tree/hooks payload
//! into a local build context next to the generated Containerfile, then
//! packages the result as a sibling tarball. The emitted Containerfile
//! references no registry image except the base.

use crate::loader::LoadedFragment;
use crate::manifest::FragmentSource;
use crate::mount::MountMaterialization;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Filename of the sentinel marker written into every self-contained output
/// directory. Its presence, not directory contents, is the ownership proof
/// `check_target_dir_safe` relies on: a directory containing a `Containerfile`
/// and `fragments/` for reasons of its own (a false positive under a
/// content-only heuristic) will not coincidentally contain this exact
/// dotfile. The sentinel is a regular file within `<dir>`, so it is part of
/// the committed tree and the packaged archive like everything else the tool
/// writes; a directory checked out from git with the sentinel intact is
/// exactly as regenerable as the one that produced it.
pub const SENTINEL_FILENAME: &str = ".osfragment-assemble";

/// Contents written to the sentinel file: tool name and version, nothing
/// else. Presence is what `check_target_dir_safe` checks, not this exact
/// text, so the format has no compatibility contract to keep.
pub fn sentinel_contents() -> String {
    format!("osfragment-assemble v{}\n", env!("CARGO_PKG_VERSION"))
}

/// Filename of the ignore file `--materialize-mounts` writes into a context
/// that holds mount material.
const GITIGNORE_FILENAME: &str = ".gitignore";

/// Contents of that file: the pattern plus the reason it is there.
///
/// Git does not record file modes, so the owner-only protection applied to
/// the mount subtrees does not survive a commit. The consequence is
/// deliberate: a committed context omits its mount material and fails loudly
/// at build time, at the mount source, and this comment is what makes that
/// failure read as policy rather than mystery.
fn mount_gitignore_contents() -> &'static str {
    "# Written by osfragment-assemble.\n\
     #\n\
     # This build context holds build-mount material under fragments/*/mount/,\n\
     # written owner-only on disk. Git does not record file modes, so committing\n\
     # it would publish that material world-readable. While it holds mount\n\
     # material this context is for direct handoff, not for committing.\n\
     #\n\
     # A committed context omits the material, and the build then fails at the\n\
     # mount source rather than building without it.\n\
     fragments/*/mount/\n"
}

/// Entries the tool itself may have written to a self-contained output
/// directory in a prior run. A directory is safe to regenerate only if
/// every entry it contains is one of these.
const TOOL_GENERATED_ENTRIES: &[&str] = &[
    "Containerfile",
    "manifest.yaml",
    "fragments",
    SENTINEL_FILENAME,
    GITIGNORE_FILENAME,
];

/// Permission mode applied to the output directory after the staging swap,
/// overriding the 0700 the staging tempdir was created with. `<dir>` is a
/// handoff artifact (committed to git, packaged into a tarball for other
/// pipelines), not a private scratch directory, so it and the resulting
/// tar entries should be normally readable.
const OUTPUT_DIR_MODE: u32 = 0o755;

/// Permission mode applied to every directory in a materialized `mount/`
/// subtree: owner only. An explicit exception to `OUTPUT_DIR_MODE`'s
/// normally readable handoff contract, because mount material is credential
/// material more often than not and the context and its archive are a
/// durable copy of it.
const MOUNT_DIR_MODE: u32 = 0o700;

/// Permission mode applied to every regular file in a materialized `mount/`
/// subtree: owner read/write only. Credential-bearing mount files are
/// normalized to this mode regardless of what the fragment's own layer
/// carried; the same intentional exception to `OUTPUT_DIR_MODE`'s normally
/// readable handoff contract that governs `MOUNT_DIR_MODE`, applied to
/// files rather than directories.
const MOUNT_FILE_MODE: u32 = 0o600;

/// Refuse to regenerate into a directory that holds anything the tool did
/// not write itself. Absent and empty directories are always safe; a
/// directory containing the sentinel file (`.osfragment-assemble`) and no
/// entries outside the tool-generated set is recognized as tool-generated
/// from a prior run and is safe to delete and recreate. The sentinel, not
/// the presence of `Containerfile`/`fragments/` alone, is what proves
/// ownership: those names are common enough that a user's own directory
/// could coincidentally match them, but not this exact dotfile.
pub fn check_target_dir_safe(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if !dir.is_dir() {
        bail!(
            "--self-contained target {} exists and is not a directory; point \
             --self-contained at a directory path instead",
            dir.display()
        );
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        entries.push(entry?.file_name().to_string_lossy().to_string());
    }

    if entries.is_empty() {
        return Ok(());
    }

    let all_recognized = entries
        .iter()
        .all(|e| TOOL_GENERATED_ENTRIES.contains(&e.as_str()));
    let has_sentinel = entries.iter().any(|e| e == SENTINEL_FILENAME);

    if all_recognized && has_sentinel {
        return Ok(());
    }

    bail!(
        "--self-contained target {} already exists and was not generated by this tool \
         (expected the {} sentinel plus only tool-generated entries, and nothing else); \
         point --self-contained at a new or empty directory",
        dir.display(),
        SENTINEL_FILENAME
    );
}

/// Refuse a target path that is itself a symlink, before anything
/// destructive runs.
///
/// Every other check here goes through `exists()`/`is_dir()`, which resolve
/// a symlink to its target. That makes a symlinked `<dir>` ambiguous in a
/// way worth refusing outright rather than resolving silently: the safety
/// check would read the *target's* contents to authorize the write, while
/// the path actually removed and replaced is the link. A dangling symlink
/// is the mirror image, reading as absent and failing only at the final
/// rename. Both are refused here with a message that names the symlink.
fn check_target_not_symlink(dir: &Path) -> Result<()> {
    let is_symlink = fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink());
    if is_symlink {
        bail!(
            "--self-contained target {} is a symlink; point it at a real directory path \
             (this mode replaces the target directory wholesale and will not write \
             through a symlink)",
            dir.display()
        );
    }
    Ok(())
}

/// Set every directory in `<fragment_dir>/mount` to [`MOUNT_DIR_MODE`] and
/// every regular file in it to [`MOUNT_FILE_MODE`].
///
/// A no-op when the fragment carries no mount material. Each directory is
/// read, and its files chmod'd, before the directory itself is restricted,
/// so tightening a parent never blocks the walk into its children or the
/// chmod of the files inside it. A 0700 directory only blocks live
/// traversal; the sibling archive records each file's own mode
/// independently, so leaving files at whatever the fragment's layer carried
/// would still expose credential material once extracted elsewhere.
#[cfg(unix)]
fn restrict_mount_tree(fragment_dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mount_root = fragment_dir.join("mount");
    if !mount_root.is_dir() {
        return Ok(());
    }
    let mut stack = vec![mount_root];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                fs::set_permissions(&path, fs::Permissions::from_mode(MOUNT_FILE_MODE))
                    .with_context(|| format!("restricting permissions on {}", path.display()))?;
            }
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(MOUNT_DIR_MODE))
            .with_context(|| format!("restricting permissions on {}", dir.display()))?;
    }
    Ok(())
}

/// File modes are a Unix concept; elsewhere the materialized tree carries
/// whatever the platform gives it.
#[cfg(not(unix))]
fn restrict_mount_tree(_fragment_dir: &Path) -> Result<()> {
    Ok(())
}

/// Materialize the self-contained output: fragment tree/hooks payload,
/// the generated Containerfile, and a copy of the input manifest.
///
/// Builds into a staging directory next to `dir` first and swaps it into
/// place only after every fragment materializes successfully, so a
/// registry failure partway through never leaves a partial tree at `dir`.
///
/// The staging directory is reclaimed automatically on every failure up to
/// and including the removal of an existing `dir`, leaving `dir` untouched.
/// That is where the guarantee ends: `keep()` disarms the cleanup so the
/// tree can survive the rename, so a failing rename leaves the complete
/// output at the staging path and says so in its error rather than
/// discarding minutes of registry work.
///
/// `materialize` is the per-fragment materialization call; production
/// code always passes `crate::loader::materialize_fragment` (see
/// `write_output` below), tests substitute a network-free stub.
fn write_output_with(
    dir: &Path,
    manifest_path: &Path,
    containerfile: &str,
    fragments: &[LoadedFragment],
    mounts: MountMaterialization,
    materialize: impl Fn(&str, &Path) -> Result<()>,
) -> Result<()> {
    check_target_not_symlink(dir)?;
    check_target_dir_safe(dir)?;

    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    // Staging MUST live in <dir>'s parent, not $TMPDIR: fs::rename below is
    // only atomic within a filesystem, and the whole no-partial-state
    // guarantee rests on that rename being atomic. Relocating this breaks
    // the contract silently, with no test failure.
    let staging = tempfile::Builder::new()
        .prefix(".osfragment-assemble-staging-")
        .tempdir_in(parent)
        .context("creating staging directory for self-contained output")?;

    fs::write(staging.path().join("Containerfile"), containerfile)
        .context("writing staged Containerfile")?;
    fs::copy(manifest_path, staging.path().join("manifest.yaml")).with_context(|| {
        format!(
            "copying manifest {} into staged output",
            manifest_path.display()
        )
    })?;
    // The sentinel is a regular file in the staged tree, so it is part of
    // the committed tree and the archive like everything else here, and
    // check_target_dir_safe recognizes it on the next regeneration.
    fs::write(staging.path().join(SENTINEL_FILENAME), sentinel_contents())
        .context("writing sentinel marker")?;

    let staged_fragments = staging.path().join("fragments");
    fs::create_dir_all(&staged_fragments)
        .with_context(|| format!("creating {}", staged_fragments.display()))?;

    for loaded in fragments {
        let FragmentSource::Registry { ref image_ref } = loaded.source;
        let dest = staged_fragments.join(&loaded.fragment.name);
        materialize(image_ref, &dest)
            .with_context(|| format!("materializing fragment '{}'", loaded.fragment.name))?;
        if mounts == MountMaterialization::Write {
            restrict_mount_tree(&dest).with_context(|| {
                format!(
                    "restricting mount material for fragment '{}'",
                    loaded.fragment.name
                )
            })?;
        }
    }

    // Only when the context actually holds mount material: an ignore file
    // announcing credential material in a context that has none would be a
    // lie, and it would make an ordinary context look uncommittable.
    let holds_mount_material = mounts == MountMaterialization::Write
        && fragments.iter().any(|f| !f.mount_points.is_empty());
    if holds_mount_material {
        fs::write(
            staging.path().join(GITIGNORE_FILENAME),
            mount_gitignore_contents(),
        )
        .context("writing the mount material .gitignore")?;
    }

    // The staging tempdir was created at 0700. Normalize it to a normal,
    // world-readable mode: `<dir>` is a handoff artifact (committed to git,
    // packaged into the tarball below), not a private scratch directory, and
    // a 0700 top-level entry would carry into every tar header
    // create_archive writes. The mode is set on the staging path, before
    // keep(), rather than on `<dir>` after the rename: it survives the
    // rename either way, and doing it here keeps the rename the last
    // fallible step in this function.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(OUTPUT_DIR_MODE))
            .with_context(|| format!("normalizing permissions on {}", staging.path().display()))?;
    }

    // Re-check immediately before the destructive step: the checks above ran
    // before minutes of registry I/O, and <dir> may have changed since.
    check_target_not_symlink(dir)?;
    check_target_dir_safe(dir)?;

    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("removing existing {}", dir.display()))?;
    }
    // TempDir::into_path() is deprecated in favor of keep() as of tempfile
    // 3.14; both disarm the automatic cleanup so the directory survives the
    // rename below. keep() is the non-deprecated spelling.
    let staging_path = staging.keep();
    fs::rename(&staging_path, dir).with_context(|| {
        format!(
            "moving staged output {} into {}; the generated output is complete \
             and left at the staging path",
            staging_path.display(),
            dir.display()
        )
    })?;

    Ok(())
}

/// Materialize the self-contained output at `dir` using the real registry
/// pull path.
pub fn write_output(
    dir: &Path,
    manifest_path: &Path,
    containerfile: &str,
    fragments: &[LoadedFragment],
    mounts: MountMaterialization,
) -> Result<()> {
    write_output_with(
        dir,
        manifest_path,
        containerfile,
        fragments,
        mounts,
        |image_ref, dest| crate::loader::materialize_fragment(image_ref, dest, mounts),
    )
}

/// Sibling archive path for a self-contained output directory: `<dir>`
/// with `.tar.gz` appended to its final component, e.g. `build/context` ->
/// `build/context.tar.gz`. Derives the name from `Path::file_name()`
/// rather than raw `OsStr` concatenation, so a trailing separator (e.g.
/// `--self-contained out/`, what shell completion produces for an existing
/// directory) still yields the sibling `out.tar.gz` rather than a hidden
/// `.tar.gz` file inside `out/`.
fn archive_path_for(dir: &Path) -> PathBuf {
    let file_name = dir.file_name().unwrap_or(dir.as_os_str());
    let mut archive_name = file_name.to_os_string();
    archive_name.push(".tar.gz");
    match dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(archive_name),
        _ => PathBuf::from(archive_name),
    }
}

/// Package `dir` as a sibling `.tar.gz`, with a single top-level directory
/// named after `dir`'s basename so extraction is predictable regardless of
/// where the archive is unpacked. Returns the archive's path.
///
/// Builds the gzip/tar stream into a temp file beside the destination and
/// renames it into place as the last fallible step, mirroring
/// `write_output_with`'s staging contract. On any failure (a walk error, a
/// full disk, the gzip/tar finish failing) the temp file is dropped and
/// reclaimed automatically, and a pre-existing `<dir>.tar.gz` is left
/// untouched rather than truncated. The rename also replaces a symlinked
/// `<dir>.tar.gz` outright instead of writing through it.
pub fn create_archive(dir: &Path) -> Result<PathBuf> {
    let file_name = dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory name", dir.display()))?;
    let archive_path = archive_path_for(dir);
    let parent = archive_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    // Staging MUST live in the archive's parent, not $TMPDIR: persist below
    // is only atomic within a filesystem, and the whole no-partial-state
    // guarantee rests on that persist being atomic. Relocating this breaks
    // the contract silently, with no test failure.
    let staged = tempfile::Builder::new()
        .prefix(".osfragment-assemble-archive-")
        .tempfile_in(parent)
        .with_context(|| format!("creating staging archive in {}", parent.display()))?;

    let encoder = flate2::write::GzEncoder::new(staged, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.append_dir_all(file_name, dir).with_context(|| {
        format!(
            "archiving {} into {}",
            dir.display(),
            archive_path.display()
        )
    })?;
    let staged = builder
        .into_inner()
        .context("finishing tar stream")?
        .finish()
        .context("finishing gzip stream")?;

    staged.persist(&archive_path).map_err(|e| {
        anyhow::anyhow!(
            "moving staged archive into {}: {}",
            archive_path.display(),
            e
        )
    })?;

    Ok(archive_path)
}

/// Refuse to write build-mount material into a self-contained context
/// unless the user asked for it by name.
///
/// Self-contained output carries no registry references by design, so mount
/// material cannot arrive as an inline `from=` and has to be materialized
/// into the context and its sibling archive. That is a custody change rather
/// than a packaging detail, which is why it is gated rather than noticed.
pub fn check_mount_materialization(
    dir: &Path,
    fragments: &[LoadedFragment],
    materialize_mounts: bool,
) -> Result<()> {
    if materialize_mounts {
        return Ok(());
    }

    let mut landing: Vec<String> = Vec::new();
    for loaded in fragments {
        for point in &loaded.mount_points {
            landing.push(format!(
                "  {} (fragment '{}')",
                dir.join(point.context_source(&loaded.fragment.name))
                    .display(),
                loaded.fragment.name
            ));
        }
    }
    if landing.is_empty() {
        return Ok(());
    }

    bail!(
        "--self-contained would write build-mount material into the build context. \
         These paths would land on disk:\n{}\n\
         and the same bytes would be copied into the sibling archive {}. Self-contained \
         output carries no registry references, so mount material cannot arrive by \
         reference and has to be materialized. Treat that as a custody change: the copy \
         is durable, and git does not record file modes, so committing the context would \
         publish owner-only material world-readable. If that is what you intend, re-run \
         with --materialize-mounts, which writes the mount subtrees owner-only and adds a \
         .gitignore covering fragments/*/mount/. Otherwise generate without \
         --self-contained, where the material is mounted from a digest-pinned reference \
         instead of being materialized into the context.",
        landing.join("\n"),
        archive_path_for(dir).display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_directory_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("does-not-exist-yet");
        assert!(check_target_dir_safe(&dir).is_ok());
    }

    #[test]
    fn empty_directory_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("empty");
        fs::create_dir(&dir).unwrap();
        assert!(check_target_dir_safe(&dir).is_ok());
    }

    #[test]
    fn tool_generated_directory_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prior-run");
        fs::create_dir_all(dir.join("fragments/epel")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        fs::write(dir.join("manifest.yaml"), "base: x\n").unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();
        assert!(check_target_dir_safe(&dir).is_ok());
    }

    #[test]
    fn existing_file_target_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("not-a-dir");
        fs::write(&path, b"user data").unwrap();
        let err = check_target_dir_safe(&path).unwrap_err();
        assert!(err.to_string().contains("is not a directory"));
        assert_eq!(fs::read(&path).unwrap(), b"user data");
    }

    #[test]
    fn foreign_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mine");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("README.md"), "not ours").unwrap();
        let err = check_target_dir_safe(&dir).unwrap_err();
        assert!(err.to_string().contains("was not generated by this tool"));
    }

    #[test]
    fn directory_missing_sentinel_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("partial");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        let err = check_target_dir_safe(&dir).unwrap_err();
        assert!(err.to_string().contains("was not generated by this tool"));
    }

    #[test]
    fn containerfile_and_fragments_without_sentinel_is_refused() {
        // The exact false positive the sentinel replaces: a user's own
        // directory that happens to contain both a Containerfile and a
        // fragments/ subdirectory for unrelated reasons must not be treated
        // as tool-generated just because a content-only heuristic would
        // have matched it.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("users-own-project");
        fs::create_dir_all(dir.join("fragments/whatever")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM my-own-base\n").unwrap();
        let err = check_target_dir_safe(&dir).unwrap_err();
        assert!(err.to_string().contains("was not generated by this tool"));
    }

    #[test]
    fn sentinel_present_but_extra_user_file_is_refused() {
        // The sentinel proves the tool wrote *something* here at some
        // point, but an unexpected extra entry alongside it is still
        // refused rather than silently swept up in the next regeneration.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prior-run-plus-extra");
        fs::create_dir_all(dir.join("fragments/epel")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();
        fs::write(dir.join("notes.txt"), "my own notes").unwrap();
        let err = check_target_dir_safe(&dir).unwrap_err();
        assert!(err.to_string().contains("was not generated by this tool"));
    }

    fn test_loaded_fragment(name: &str) -> LoadedFragment {
        use crate::fragment::{
            Fragment, FragmentConflicts, FragmentName, FragmentPackages, FragmentProvides,
        };
        LoadedFragment {
            fragment: Fragment {
                name: FragmentName::new(name).expect("test fragment name must be valid"),
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
                image_ref: format!("quay.io/test/{}:1", name),
            },
            resolved_digest: None,
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
            mount_points: vec![],
            has_mount_dir: false,
            drift_warning: None,
        }
    }

    fn mount_carrying_fragment(name: &str, mount_files: &[&str]) -> LoadedFragment {
        let mut loaded = test_loaded_fragment(name);
        let files: Vec<PathBuf> = mount_files.iter().map(PathBuf::from).collect();
        loaded.mount_points =
            crate::mount::derive_mount_points(name, &files).expect("fixture derives");
        loaded
    }

    #[test]
    fn self_contained_refuses_mount_material_without_the_flag() {
        let fragments = vec![mount_carrying_fragment(
            "rhel-entitlement",
            &["etc/pki/entitlement/cert.pem"],
        )];
        let err = check_mount_materialization(Path::new("out"), &fragments, false)
            .expect_err("materializing credential material is a custody change")
            .to_string();

        assert!(
            err.contains("rhel-entitlement"),
            "names the fragment: {err}"
        );
        assert!(
            err.contains("out/fragments/rhel-entitlement/mount/etc/pki/entitlement"),
            "names the exact path that would land on disk: {err}"
        );
        assert!(
            err.contains("out.tar.gz"),
            "names the sibling archive: {err}"
        );
        assert!(
            err.contains("--materialize-mounts"),
            "names the flag: {err}"
        );
        // The specific clause, not just "git": the message also mentions git
        // when it points at --materialize-mounts and its .gitignore, so a
        // bare contains("git") stays green even if the reason committing the
        // context is dangerous were dropped from the message entirely.
        assert!(
            err.contains("git does not record file modes"),
            "leads with the custody change and the not-for-git rationale: {err}"
        );
    }

    #[test]
    fn self_contained_proceeds_with_the_flag() {
        let fragments = vec![mount_carrying_fragment(
            "rhel-entitlement",
            &["etc/pki/entitlement/cert.pem"],
        )];
        assert!(check_mount_materialization(Path::new("out"), &fragments, true).is_ok());
    }

    #[test]
    fn a_composition_without_mount_material_is_never_gated() {
        let fragments = vec![test_loaded_fragment("epel")];
        assert!(check_mount_materialization(Path::new("out"), &fragments, false).is_ok());
    }

    #[test]
    fn the_gate_names_every_offending_fragment_and_path() {
        let fragments = vec![
            mount_carrying_fragment("entitlement", &["etc/pki/entitlement/c.pem"]),
            mount_carrying_fragment("mirror", &["etc/pki/tls/mirror/client.pem"]),
        ];
        let err = check_mount_materialization(Path::new("build/ctx"), &fragments, false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("build/ctx/fragments/entitlement/mount/etc/pki/entitlement"),
            "{err}"
        );
        assert!(
            err.contains("build/ctx/fragments/mirror/mount/etc/pki/tls/mirror"),
            "{err}"
        );
    }

    /// Builds a minimal fragment layer tarball for tests that need to
    /// exercise the real `extract_fragment_payload_to_disk` extractor
    /// without a registry. Mirrors the real layer layout
    /// (`fragment/tree/...`, `fragment/hooks/...`). Takes an explicit mode
    /// per entry (following the `RawEntry`/`create_test_tarball_with_modes`
    /// precedent in `src/loader.rs`) so tests can assert modes survive in
    /// both directions rather than everything landing at one fixed mode.
    fn build_fixture_layer(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for (path, data, mode) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_path(path).unwrap();
                header.set_size(data.len() as u64);
                header.set_mode(*mode);
                header.set_cksum();
                tar.append(&header, &data[..]).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        buf
    }

    #[test]
    fn materialized_mount_directories_and_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        // The mount file's source mode is deliberately permissive (0o644) so
        // this test proves restrict_mount_tree normalizes it to 0o600 rather
        // than merely preserving an already-secure input.
        let layer = build_fixture_layer(&[
            ("fragment/tree/etc/motd", b"hello", 0o644),
            (
                "fragment/mount/etc/pki/entitlement/cert.pem",
                b"secret",
                0o644,
            ),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("entitlement")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();

        let mount_root = dir.join("fragments/entitlement/mount");
        for path in [
            mount_root.clone(),
            mount_root.join("etc"),
            mount_root.join("etc/pki"),
            mount_root.join("etc/pki/entitlement"),
        ] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} must be owner-only, an explicit exception to the output \
                 directory's normally readable handoff contract",
                path.display()
            );
        }

        let cert_mode = fs::metadata(mount_root.join("etc/pki/entitlement/cert.pem"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            cert_mode, 0o600,
            "a mount file must be normalized to owner-only regardless of the \
             mode its source layer carried"
        );

        // The exception is scoped to mount/: everything else keeps the normal
        // handoff mode. Pinned to the exact mode rather than merely "not
        // 0700", so a partial widening of restrict_mount_tree's scope, which
        // would leave a non-mount path at some other tightened value, is
        // caught too.
        let tree_mode = fs::metadata(dir.join("fragments/entitlement/tree"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            tree_mode, OUTPUT_DIR_MODE,
            "tree/ is not credential material and keeps the readable handoff mode"
        );
    }

    #[test]
    fn the_archive_preserves_the_owner_only_mount_modes() {
        // The mount file's source mode is deliberately permissive (0o644):
        // the archive records each file's own mode independently of its
        // parent directory, so this proves the normalized mode survives
        // into the archive rather than merely blocking live traversal.
        let layer = build_fixture_layer(&[(
            "fragment/mount/etc/pki/entitlement/cert.pem",
            b"secret",
            0o644,
        )]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("entitlement")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();
        let archive = create_archive(&dir).unwrap();

        let bytes = fs::read(&archive).unwrap();
        let decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut tar = tar::Archive::new(decoder);
        let mut checked_dirs = 0;
        let mut checked_files = 0;
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            if !path.contains("/mount") {
                continue;
            }
            if entry.header().entry_type().is_dir() {
                assert_eq!(
                    entry.header().mode().unwrap() & 0o777,
                    0o700,
                    "archived directory {path} must carry the owner-only mode"
                );
                checked_dirs += 1;
            } else if entry.header().entry_type().is_file() {
                assert_eq!(
                    entry.header().mode().unwrap() & 0o777,
                    0o600,
                    "archived file {path} must carry the owner-only mode"
                );
                checked_files += 1;
            }
        }
        assert!(
            checked_dirs > 0,
            "the archive must contain the mount directories"
        );
        assert!(checked_files > 0, "the archive must contain the mount file");
    }

    #[test]
    fn materializing_mounts_writes_a_gitignore_that_explains_itself() {
        let layer = build_fixture_layer(&[(
            "fragment/mount/etc/pki/entitlement/cert.pem",
            b"secret",
            0o600,
        )]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![mount_carrying_fragment(
            "entitlement",
            &["etc/pki/entitlement/cert.pem"],
        )];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();

        let ignore = fs::read_to_string(dir.join(".gitignore")).expect("a .gitignore is written");
        // Written independently of mount_gitignore_contents(): the point is
        // to lock the spec's exact text, not to echo the implementation, so
        // this comparison would still catch the implementation drifting
        // away from the brief even if the constant itself did not.
        let expected = "# Written by osfragment-assemble.\n\
             #\n\
             # This build context holds build-mount material under fragments/*/mount/,\n\
             # written owner-only on disk. Git does not record file modes, so committing\n\
             # it would publish that material world-readable. While it holds mount\n\
             # material this context is for direct handoff, not for committing.\n\
             #\n\
             # A committed context omits the material, and the build then fails at the\n\
             # mount source rather than building without it.\n\
             fragments/*/mount/\n";
        assert_eq!(
            ignore, expected,
            "the .gitignore text is the output contract; any wording, line-break, \
             or punctuation drift, including an em dash slipping in, changes it \
             silently unless this comparison is exact"
        );
    }

    #[test]
    fn a_run_without_mount_material_writes_no_gitignore() {
        let layer = build_fixture_layer(&[("fragment/tree/etc/motd", b"hello", 0o644)]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("epel")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();

        assert!(
            !dir.join(".gitignore").exists(),
            "a context holding no mount material is committable, and an ignore \
             file claiming otherwise would be a lie"
        );
    }

    #[test]
    fn a_context_carrying_a_gitignore_is_still_regenerable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prior-run");
        fs::create_dir_all(dir.join("fragments/entitlement")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        fs::write(dir.join("manifest.yaml"), "base: x\n").unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();
        fs::write(dir.join(".gitignore"), mount_gitignore_contents()).unwrap();
        assert!(
            check_target_dir_safe(&dir).is_ok(),
            "the tool writes this file, so it must recognize it on the next run"
        );
    }

    #[test]
    fn materialize_and_archive_round_trip_byte_for_byte() {
        // Composes write_output_with (real extract_fragment_payload_to_disk,
        // stubbing only the skopeo pull) with create_archive over the same
        // tree: the spec's single acceptance test 2 (materialize from
        // fixture fragments, then diff the archive against the tree byte
        // for byte), with no network access.
        let epel_layer = build_fixture_layer(&[(
            "fragment/tree/etc/yum.repos.d/epel.repo",
            b"[epel]\nbaseurl=https://example.com/epel/\n",
            0o755,
        )]);
        let cis_layer = build_fixture_layer(&[
            (
                "fragment/tree/usr/lib/sysctl.d/99-hardening.conf",
                b"kernel.randomize_va_space=2\n",
                0o755,
            ),
            (
                "fragment/hooks/configure.sh",
                b"#!/bin/sh\necho hi\n",
                0o755,
            ),
        ]);

        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("epel"), test_loaded_fragment("cis")];
        write_output_with(
            &dir,
            &manifest_path,
            "FROM example\n",
            &fragments,
            MountMaterialization::Skip,
            |image_ref, dest| {
                let layer: &[u8] = match image_ref {
                    "quay.io/test/epel:1" => &epel_layer,
                    "quay.io/test/cis:1" => &cis_layer,
                    other => panic!("unexpected image_ref in test: {other}"),
                };
                crate::loader::extract_fragment_payload_to_disk(
                    layer,
                    dest,
                    MountMaterialization::Skip,
                )
            },
        )
        .unwrap();

        let archive_path = create_archive(&dir).unwrap();

        let extract_dir = workdir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        let file = fs::File::open(&archive_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&extract_dir).unwrap();
        let extracted_root = extract_dir.join("ctx");

        for rel in [
            "Containerfile",
            "manifest.yaml",
            SENTINEL_FILENAME,
            "fragments/epel/tree/etc/yum.repos.d/epel.repo",
            "fragments/cis/tree/usr/lib/sysctl.d/99-hardening.conf",
            "fragments/cis/hooks/configure.sh",
        ] {
            let original = fs::read(dir.join(rel)).unwrap();
            let extracted = fs::read(extracted_root.join(rel)).unwrap();
            assert_eq!(
                original, extracted,
                "{} did not round-trip byte for byte",
                rel
            );
        }
    }

    #[test]
    fn hooks_only_fragment_materializes_hooks_without_tree_dir() {
        // A fragment with hooks but no tree/ content must produce
        // fragments/<name>/hooks/ and no fragments/<name>/tree/ at all,
        // not an empty tree/ directory.
        let hooks_only_layer =
            build_fixture_layer(&[("fragment/hooks/setup.sh", b"#!/bin/sh\necho setup\n", 0o755)]);

        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("hooks-only")];
        write_output_with(
            &dir,
            &manifest_path,
            "FROM example\n",
            &fragments,
            MountMaterialization::Skip,
            |_image_ref, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &hooks_only_layer,
                    dest,
                    MountMaterialization::Skip,
                )
            },
        )
        .unwrap();

        assert!(dir.join("fragments/hooks-only/hooks/setup.sh").exists());
        assert!(
            !dir.join("fragments/hooks-only/tree").exists(),
            "a hooks-only fragment must not produce a tree/ directory"
        );
    }

    /// Builds a `LoadedFragment` whose metadata paths are derived from its
    /// fixture layer entries the same way `load_registry_fragment` derives
    /// them from a real layer: `tree_paths` relative to `fragment/`,
    /// `hook_paths` relative to `fragment/hooks/`. The fixture's tar entries
    /// stay the single source of truth for both the metadata the generator
    /// emits from and the bytes `extract_fragment_payload_to_disk` writes,
    /// which is what keeps the seam test below from confirming itself.
    fn fixture_fragment(
        name: &str,
        manifest_index: usize,
        entries: &[(&str, &[u8], u32)],
    ) -> LoadedFragment {
        let mut loaded = test_loaded_fragment(name);
        loaded.manifest_index = manifest_index;
        loaded.tree_paths = entries
            .iter()
            .filter_map(|(p, _, _)| Path::new(p).strip_prefix("fragment").ok())
            .map(Path::to_path_buf)
            .collect();
        let mut hook_paths: Vec<PathBuf> = entries
            .iter()
            .filter_map(|(p, _, _)| Path::new(p).strip_prefix("fragment/hooks").ok())
            .map(Path::to_path_buf)
            .collect();
        hook_paths.sort();
        loaded.hook_paths = hook_paths;
        loaded
    }

    /// Every build-context path an emitted Containerfile references, taken
    /// from `COPY` sources and bind-mount `source=` options alike. Splitting
    /// on whitespace and commas covers both forms in one pass; a trailing
    /// `/` is trimmed so each result can be checked against the tree
    /// directly.
    fn context_paths_referenced(containerfile: &str) -> Vec<String> {
        let mut refs = Vec::new();
        for token in containerfile.split(|c: char| c.is_whitespace() || c == ',') {
            let token = token.strip_prefix("source=").unwrap_or(token);
            if let Some(rest) = token.strip_prefix("fragments/") {
                refs.push(format!("fragments/{}", rest.trim_end_matches('/')));
            }
        }
        refs
    }

    #[test]
    fn emitted_containerfile_paths_resolve_in_the_materialized_tree() {
        // The seam that was previously verified only by inspection: the
        // generator emits `fragments/<name>/tree|hooks/...` paths from a
        // fragment's metadata, while write_output and
        // extract_fragment_payload_to_disk independently decide where those
        // files actually land. Nothing composed the two. This test generates
        // the Containerfile and materializes the same fixture layers into one
        // tree, then requires every build-context path the Containerfile
        // names to exist on disk and to be carried by the archive.
        //
        // The third fragment is hooks-only: it must contribute the hook mount
        // and no COPY, since it has no tree/ content to copy from.
        let epel_entries: [(&str, &[u8], u32); 2] = [
            (
                "fragment/tree/etc/yum.repos.d/epel.repo",
                b"[epel]\nbaseurl=https://example.com/epel/\n",
                0o644,
            ),
            (
                "fragment/tree/etc/pki/rpm-gpg/RPM-GPG-KEY-EPEL",
                b"-----BEGIN PGP PUBLIC KEY BLOCK-----\n",
                0o644,
            ),
        ];
        let cis_entries: [(&str, &[u8], u32); 2] = [
            (
                "fragment/tree/usr/lib/sysctl.d/99-hardening.conf",
                b"kernel.randomize_va_space=2\n",
                0o644,
            ),
            (
                "fragment/hooks/entrypoint",
                b"#!/bin/sh\necho configure\n",
                0o755,
            ),
        ];
        let hooks_only_entries: [(&str, &[u8], u32); 1] = [(
            "fragment/hooks/entrypoint",
            b"#!/bin/sh\necho setup\n",
            0o755,
        )];

        let fragments = vec![
            fixture_fragment("epel", 0, &epel_entries),
            fixture_fragment("cis", 1, &cis_entries),
            fixture_fragment("hooks-only", 2, &hooks_only_entries),
        ];

        let manifest = crate::manifest::Manifest {
            base: "registry.example/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![
                crate::manifest::ManifestFragment {
                    image: "quay.io/test/epel:1".into(),
                    packages: vec![],
                    mirror: None,
                },
                crate::manifest::ManifestFragment {
                    image: "quay.io/test/cis:1".into(),
                    packages: vec![],
                    mirror: None,
                },
                crate::manifest::ManifestFragment {
                    image: "quay.io/test/hooks-only:1".into(),
                    packages: vec![],
                    mirror: None,
                },
            ],
        };
        let containerfile =
            crate::generator::generate_containerfile(&manifest, &fragments, None, false, true)
                .unwrap();

        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        // Deliberately not named osfragment-assemble.yaml: the copy must land
        // at the fixed name regardless of what the input was called.
        let manifest_path = workdir.path().join("my-composition.yml");
        fs::write(&manifest_path, "base: registry.example/base:1\n").unwrap();

        write_output_with(
            &dir,
            &manifest_path,
            &containerfile,
            &fragments,
            MountMaterialization::Skip,
            |image_ref, dest| {
                let entries: &[(&str, &[u8], u32)] = match image_ref {
                    "quay.io/test/epel:1" => &epel_entries,
                    "quay.io/test/cis:1" => &cis_entries,
                    "quay.io/test/hooks-only:1" => &hooks_only_entries,
                    other => panic!("unexpected image_ref in test: {other}"),
                };
                crate::loader::extract_fragment_payload_to_disk(
                    &build_fixture_layer(entries),
                    dest,
                    MountMaterialization::Skip,
                )
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("manifest.yaml")).unwrap(),
            "base: registry.example/base:1\n",
            "the input manifest must be copied under the fixed manifest.yaml name"
        );

        let refs = context_paths_referenced(&containerfile);
        assert!(
            refs.len() >= 5,
            "expected the repo, rpm-gpg, payload and both hook references, got {refs:?}"
        );
        for reference in &refs {
            assert!(
                dir.join(reference).exists(),
                "Containerfile references {reference}, which materialization did not produce"
            );
        }

        // A hooks-only fragment has nothing to COPY: referencing a tree/
        // directory that materialization correctly never created would fail
        // the build at COPY time.
        assert!(
            !containerfile.contains("fragments/hooks-only/tree"),
            "a hooks-only fragment must not get a COPY of a tree/ that does not exist"
        );
        assert!(
            refs.contains(&"fragments/hooks-only/hooks".to_string()),
            "a hooks-only fragment must still get its hook mount, got {refs:?}"
        );

        // The hook mount's target is /frag-hooks, so each fragment's
        // invocation must resolve to a real file under that fragment's
        // materialized hooks/ directory.
        let lines: Vec<&str> = containerfile.lines().collect();
        let mut checked_hooks = 0;
        for (idx, line) in lines.iter().enumerate() {
            let Some((_, rest)) = line.split_once("source=fragments/") else {
                continue;
            };
            let frag_name = rest
                .split('/')
                .next()
                .expect("split always yields one part");
            let command_line = lines
                .get(idx + 1)
                .expect("a hook mount line is always followed by its command line");
            for token in command_line.split_whitespace() {
                if let Some(hook) = token.strip_prefix("/frag-hooks/") {
                    let on_disk = dir
                        .join("fragments")
                        .join(frag_name)
                        .join("hooks")
                        .join(hook);
                    assert!(
                        on_disk.exists(),
                        "hook command /frag-hooks/{hook} resolves to {}, which does not exist",
                        on_disk.display()
                    );
                    checked_hooks += 1;
                }
            }
        }
        assert_eq!(
            checked_hooks, 2,
            "both hook-bearing fragments' commands must have been checked"
        );

        // The archive is the handoff artifact, so it has to carry every
        // context path the Containerfile needs, not just the tree does.
        let archive_path = create_archive(&dir).unwrap();
        let file = fs::File::open(&archive_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        let archived: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path().unwrap().to_path_buf();
                path.strip_prefix("ctx")
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .trim_end_matches('/')
                    .to_string()
            })
            .collect();
        for reference in &refs {
            assert!(
                archived
                    .iter()
                    .any(|e| e == reference || e.starts_with(&format!("{reference}/"))),
                "archive is missing {reference}, which the Containerfile references"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn hook_exec_bit_survives_materialization_and_archive() {
        // Carried forward from Task 5 (unexpressible before Task 4's fix
        // round added a mode-aware tarball builder): a hook file's
        // executable bit must survive both extract_fragment_payload_to_disk
        // and the tar round-trip through create_archive, not just its byte
        // content, since hooks are executed directly from the extracted
        // archive. Bidirectional per review Minor 3: also pins a non-
        // executable tree file at 0o644 so a blanket mode promotion (e.g.
        // everything landing at 0o755) would be caught too, not just a
        // dropped exec bit.
        use std::os::unix::fs::PermissionsExt;

        let layer = build_fixture_layer(&[
            ("fragment/hooks/setup.sh", b"#!/bin/sh\necho setup\n", 0o755),
            ("fragment/tree/etc/app.conf", b"key=value\n", 0o644),
        ]);

        let workdir = tempfile::tempdir().unwrap();
        let dest = workdir.path().join("frag");
        crate::loader::extract_fragment_payload_to_disk(&layer, &dest, MountMaterialization::Skip)
            .unwrap();

        let hook_path = dest.join("hooks/setup.sh");
        let hook_mode = fs::metadata(&hook_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            hook_mode, 0o755,
            "hook exec bit lost during materialization"
        );

        let conf_path = dest.join("tree/etc/app.conf");
        let conf_mode = fs::metadata(&conf_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            conf_mode, 0o644,
            "non-executable tree file must not be promoted to executable during materialization"
        );

        let archive_path = create_archive(&dest).unwrap();
        let extract_dir = workdir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        let file = fs::File::open(&archive_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&extract_dir).unwrap();

        let extracted_hook = extract_dir.join("frag/hooks/setup.sh");
        let hook_mode_in_archive =
            fs::metadata(&extracted_hook).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            hook_mode_in_archive, 0o755,
            "hook exec bit lost through the create_archive round trip"
        );

        let extracted_conf = extract_dir.join("frag/tree/etc/app.conf");
        let conf_mode_in_archive =
            fs::metadata(&extracted_conf).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            conf_mode_in_archive, 0o644,
            "non-executable tree file must not be promoted to executable through create_archive"
        );
    }

    #[test]
    fn archive_path_appends_suffix_without_touching_dots() {
        assert_eq!(
            archive_path_for(Path::new("build/context")),
            PathBuf::from("build/context.tar.gz")
        );
        assert_eq!(
            archive_path_for(Path::new("out.v2")),
            PathBuf::from("out.v2.tar.gz")
        );
    }

    #[test]
    fn archive_path_normalizes_trailing_separator() {
        // Regression test: --self-contained out/ (what shell completion
        // produces for an existing directory) must still yield the
        // sibling out.tar.gz, not a hidden .tar.gz file inside out/ that
        // the next run's check_target_dir_safe would then refuse as
        // foreign.
        assert_eq!(
            archive_path_for(Path::new("out/")),
            PathBuf::from("out.tar.gz")
        );
        assert_eq!(
            archive_path_for(Path::new("build/context/")),
            PathBuf::from("build/context.tar.gz")
        );
    }

    #[test]
    fn archive_contents_match_tree_byte_for_byte() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("myctx");
        fs::create_dir_all(dir.join("fragments/epel/tree/etc/yum.repos.d")).unwrap();
        fs::create_dir_all(dir.join("fragments/cis/hooks")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM registry.example/base:1\n").unwrap();
        fs::write(
            dir.join("manifest.yaml"),
            "apiVersion: osfragment/v1alpha1\n",
        )
        .unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();
        fs::write(
            dir.join("fragments/epel/tree/etc/yum.repos.d/epel.repo"),
            "[epel]\nbaseurl=https://example.com/epel/\n",
        )
        .unwrap();
        fs::write(
            dir.join("fragments/cis/hooks/configure.sh"),
            "#!/bin/sh\necho hi\n",
        )
        .unwrap();

        let archive_path = create_archive(&dir).unwrap();
        assert_eq!(
            archive_path,
            PathBuf::from(format!("{}.tar.gz", dir.display()))
        );

        let extract_dir = workdir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        let file = fs::File::open(&archive_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&extract_dir).unwrap();

        let extracted_root = extract_dir.join("myctx");
        for rel in [
            "Containerfile",
            "manifest.yaml",
            SENTINEL_FILENAME,
            "fragments/epel/tree/etc/yum.repos.d/epel.repo",
            "fragments/cis/hooks/configure.sh",
        ] {
            let original = fs::read(dir.join(rel)).unwrap();
            let extracted = fs::read(extracted_root.join(rel)).unwrap();
            assert_eq!(
                original, extracted,
                "{} did not round-trip byte for byte",
                rel
            );
        }
    }

    #[test]
    fn create_archive_leaves_previous_archive_intact_on_failure() {
        // Review Critical 1: create_archive must stage-then-rename like
        // write_output_with, not truncate the destination up front. `dir`
        // is never created, so append_dir_all fails partway through and
        // the pre-existing archive must survive byte for byte.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let archive_path = archive_path_for(&dir);
        let previous_contents = b"a previous good archive, must survive";
        fs::write(&archive_path, previous_contents).unwrap();

        let result = create_archive(&dir);
        assert!(
            result.is_err(),
            "archiving a nonexistent directory must fail"
        );

        let after = fs::read(&archive_path).unwrap();
        assert_eq!(
            after, previous_contents,
            "a failed create_archive must not touch a pre-existing archive"
        );
    }

    #[test]
    #[cfg(unix)]
    fn create_archive_replaces_a_symlinked_archive_path_rather_than_writing_through_it() {
        // Review Critical 1, consequence (b): a pre-existing symlink at
        // <dir>.tar.gz must be replaced by the rename, not written through
        // to whatever it points at.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Containerfile"), "FROM example\n").unwrap();

        let archive_path = archive_path_for(&dir);
        let decoy = workdir.path().join("decoy-target");
        fs::write(&decoy, b"decoy contents, must not be overwritten").unwrap();
        std::os::unix::fs::symlink(&decoy, &archive_path).unwrap();

        let result_path = create_archive(&dir).unwrap();
        assert_eq!(result_path, archive_path);

        let decoy_contents = fs::read(&decoy).unwrap();
        assert_eq!(
            decoy_contents, b"decoy contents, must not be overwritten",
            "the symlink's target must not be written through"
        );

        let archive_meta = fs::symlink_metadata(&archive_path).unwrap();
        assert!(
            !archive_meta.file_type().is_symlink(),
            "the symlinked archive path must be replaced with a regular file, not written through"
        );
    }

    #[test]
    fn write_output_stages_then_swaps_atomically() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("epel"), test_loaded_fragment("cis")];
        write_output_with(
            &dir,
            &manifest_path,
            "FROM example\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "FROM example\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("manifest.yaml")).unwrap(),
            "base: example\n"
        );
        assert!(dir.join("fragments/epel").is_dir());
        assert!(dir.join("fragments/cis").is_dir());
        assert!(
            dir.join(SENTINEL_FILENAME).exists(),
            "sentinel must be written into the output tree"
        );
    }

    #[test]
    fn write_output_replaces_prior_tool_generated_directory() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(dir.join("fragments/old-frag")).unwrap();
        fs::write(dir.join("Containerfile"), "OLD CONTENT\n").unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        write_output_with(
            &dir,
            &manifest_path,
            "NEW CONTENT\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "NEW CONTENT\n"
        );
        assert!(!dir.join("fragments/old-frag").exists());
        assert!(dir.join(SENTINEL_FILENAME).exists());
    }

    #[test]
    fn regeneration_is_idempotent_and_passes_safety_check_again() {
        // The update model's central operation: run write_output_with twice
        // against the same target with the same inputs. The second run must
        // pass check_target_dir_safe against the first run's own output (the
        // whole point of the sentinel) and must produce byte-identical
        // content, since output is a pure function of manifest and registry
        // state.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();
        let fragments = vec![test_loaded_fragment("epel")];

        for _ in 0..2 {
            write_output_with(
                &dir,
                &manifest_path,
                "FROM example\n",
                &fragments,
                MountMaterialization::Skip,
                |_r, d| fs::create_dir_all(d).map_err(Into::into),
            )
            .unwrap();
        }

        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "FROM example\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("manifest.yaml")).unwrap(),
            "base: example\n"
        );
        assert!(dir.join("fragments/epel").is_dir());
        assert!(dir.join(SENTINEL_FILENAME).exists());
    }

    #[test]
    fn write_output_refuses_unsafe_target_dir() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), "mine").unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        let result = write_output_with(
            &dir,
            &manifest_path,
            "X\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        );

        assert!(result.is_err());
        assert!(
            dir.join("README.md").exists(),
            "unrelated file must survive a refused run"
        );
    }

    #[test]
    fn write_output_leaves_no_partial_tree_on_materialization_failure() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(dir.join("fragments/prior-run")).unwrap();
        fs::write(dir.join("Containerfile"), "PRIOR RUN\n").unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![
            test_loaded_fragment("good-frag"),
            test_loaded_fragment("bad-frag"),
        ];
        let result = write_output_with(
            &dir,
            &manifest_path,
            "NEW\n",
            &fragments,
            MountMaterialization::Skip,
            |image_ref, dest| {
                if image_ref.contains("bad-frag") {
                    bail!("simulated registry failure");
                }
                fs::create_dir_all(dest).map_err(Into::into)
            },
        );

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "PRIOR RUN\n"
        );
        assert!(dir.join("fragments/prior-run").exists());
        assert!(
            dir.join(SENTINEL_FILENAME).exists(),
            "the prior run's sentinel must survive a failed regeneration untouched"
        );
        assert!(!dir.join("fragments/good-frag").exists());

        // The staging directory is a sibling of <dir> because that is what
        // makes the swap rename atomic (see the comment at its creation
        // site); incidentally, that also means a leaked one would litter the
        // user's working directory rather than $TMPDIR. TempDir's Drop must
        // reclaim it
        // on every failure up to and including the removal of an existing
        // <dir>, which covers a materialization failure like this one. Past
        // the removal keep() has deliberately disarmed the cleanup, so a
        // failing rename is the one case that does leave the staging tree
        // behind, with the complete output in it.
        let leaked: Vec<_> = fs::read_dir(workdir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".osfragment-assemble-staging-"))
            .collect();
        assert!(
            leaked.is_empty(),
            "staging directory leaked on the error path: {leaked:?}"
        );
    }

    #[test]
    fn write_output_refuses_when_target_gains_a_foreign_file_during_materialization() {
        // The safety checks run before minutes of registry I/O. Anything that
        // lands in <dir> while the pulls are in flight is a file the checks
        // never saw, so the re-check immediately before the destructive step
        // must refuse rather than delete it.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(&dir).unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("epel")];
        let result = write_output_with(
            &dir,
            &manifest_path,
            "NEW\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| {
                // Stands in for a concurrent writer during the pull window.
                fs::write(dir.join("README.md"), "appeared mid-run").unwrap();
                fs::create_dir_all(d).map_err(Into::into)
            },
        );

        assert!(result.is_err());
        assert!(
            dir.join("README.md").exists(),
            "a file that appeared during materialization must not be deleted"
        );
    }

    #[test]
    fn write_output_writes_only_the_tool_generated_entry_set() {
        // Anything the tool writes at the top level of <dir> outside the
        // recognized names would make check_target_dir_safe refuse the
        // tool's own output on the next regeneration. This run carries no
        // mount material and passes MountMaterialization::Skip, so the
        // deterministic expected set is every recognized entry except
        // .gitignore, which write_output_with only ever writes alongside
        // materialized mount points. Exact-set equality, not a subset
        // check, is what proves the tool writes *only* that set.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("epel")];
        write_output_with(
            &dir,
            &manifest_path,
            "FROM example\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        )
        .unwrap();

        let mut entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        entries.sort();
        let mut expected: Vec<_> = TOOL_GENERATED_ENTRIES
            .iter()
            .filter(|&&e| e != GITIGNORE_FILENAME)
            .map(|e| e.to_string())
            .collect();
        expected.sort();
        assert_eq!(entries, expected);
    }

    #[test]
    #[cfg(unix)]
    fn write_output_refuses_symlinked_target() {
        // `exists()` and `is_dir()` both resolve through a symlink, so a
        // symlinked target would otherwise pass the safety check against
        // the link's target and then have the tool remove and replace
        // whatever the link points at, outside the path the user named.
        let workdir = tempfile::tempdir().unwrap();
        let real = workdir.path().join("somewhere-else");
        fs::create_dir_all(real.join("fragments")).unwrap();
        fs::write(real.join("Containerfile"), "REAL\n").unwrap();
        fs::write(real.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();

        let dir = workdir.path().join("ctx");
        std::os::unix::fs::symlink(&real, &dir).unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        let err = write_output_with(
            &dir,
            &manifest_path,
            "NEW\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("is a symlink"),
            "error must name the symlink case, got: {err}"
        );
        assert_eq!(
            fs::read_to_string(real.join("Containerfile")).unwrap(),
            "REAL\n",
            "the symlink's target must not be touched"
        );
    }

    #[test]
    #[cfg(unix)]
    fn write_output_refuses_dangling_symlink_target() {
        // A dangling symlink reads as absent through `exists()`, so without
        // an explicit check the run would proceed all the way to the final
        // rename and fail there with a confusing ENOTDIR instead.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        std::os::unix::fs::symlink(workdir.path().join("no-such-target"), &dir).unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        let err = write_output_with(
            &dir,
            &manifest_path,
            "NEW\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("is a symlink"),
            "error must name the symlink case, got: {err}"
        );
    }

    /// Every fragment directory this function creates must be a direct child
    /// of `fragments/`. That holds because `fragment.name` is a
    /// `FragmentName`, whose grammar admits no separator and no `..`, so the
    /// join below cannot leave the staged tree. Before that type existed, a
    /// fragment named `../../escape` materialized into a sibling of the
    /// output directory entirely.
    ///
    /// The escape is now unrepresentable rather than merely unreached, so
    /// this test states the property the type is carrying; the rejection
    /// itself is pinned in `src/fragment.rs`.
    #[test]
    fn materialized_fragment_dirs_stay_inside_the_output_tree() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("epel"), test_loaded_fragment("cis")];
        let staged_dests = std::cell::RefCell::new(Vec::new());
        write_output_with(
            &dir,
            &manifest_path,
            "FROM example\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| {
                staged_dests.borrow_mut().push(d.to_path_buf());
                fs::create_dir_all(d).map_err(Into::into)
            },
        )
        .unwrap();

        for dest in staged_dests.borrow().iter() {
            assert_eq!(
                dest.parent().and_then(Path::file_name),
                Some(std::ffi::OsStr::new("fragments")),
                "{} is not a direct child of fragments/",
                dest.display()
            );
            assert!(
                !dest.to_string_lossy().contains(".."),
                "{} contains a traversal component",
                dest.display()
            );
        }
    }

    #[test]
    fn write_output_normalizes_directory_permissions() {
        // Regression test: the staging tempdir is created at 0700; without
        // an explicit fix that mode survives the rename into <dir> and
        // then into every tar header create_archive writes, which is wrong
        // for a handoff artifact meant to be committed and shared.
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        write_output_with(
            &dir,
            &manifest_path,
            "FROM example\n",
            &fragments,
            MountMaterialization::Skip,
            |_r, d| fs::create_dir_all(d).map_err(Into::into),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, OUTPUT_DIR_MODE,
                "output directory must not carry the staging tempdir's 0700"
            );
        }
    }
}
