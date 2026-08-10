use anyhow::Result;
use std::path::Path;

use crate::fragment::parse_fragment_toml;
use crate::loader::{validate_hooks_entrypoint, LoadedFragment, HOOKS_ENTRYPOINT_NAME};
use crate::mount::{derive_mount_points, empty_mount_notice, MountPoint, MOUNT_SECTION_NOTE};

pub fn run_inspect(target: &str) -> Result<()> {
    let path = Path::new(target);

    let (fragment, tree_paths, hook_paths, mount_section) = if path.is_dir() {
        let toml_path = path.join("fragment.toml");
        let content = std::fs::read_to_string(&toml_path)?;
        let frag = parse_fragment_toml(&content)?;

        let mut paths = Vec::new();
        collect_display_paths(path, "tree", &mut paths)?;

        // Hook files count at any depth, so this scan is recursive: a
        // fragment whose hooks/ holds only lib/helper.sh still needs an
        // entrypoint, and a shallow scan would pass it.
        let mut hook_list = Vec::new();
        collect_display_paths(path, "hooks", &mut hook_list)?;
        if !hook_list.is_empty() {
            validate_hooks_entrypoint(frag.name.as_str(), local_entrypoint_mode(path))?;
        }

        let mount_section = local_mount_section(path, frag.name.as_str())?;
        (frag, paths, hook_list, mount_section)
    } else {
        // Inspect requires tree/ contents and hooks: always do a
        // full load (metadata-only path skips these). The annotation
        // fast path is used inside load_registry_fragment for the
        // Fragment struct, but the layer is still pulled for tree_paths.
        let loaded = crate::loader::load_registry_fragment(target)?;
        let display_paths: Vec<String> = loaded
            .tree_paths
            .iter()
            .filter(|p| p.to_string_lossy().starts_with("tree/"))
            .map(|p| {
                p.strip_prefix("tree/")
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        let hook_list: Vec<String> = loaded
            .hook_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let mount_section = registry_mount_section(&loaded);
        (loaded.fragment, display_paths, hook_list, mount_section)
    };

    println!("Fragment: {} v{}", fragment.name, fragment.version);
    if let Some(v) = &fragment.vendor {
        println!("Vendor:   {}", v);
    }
    if !fragment.provides.repos.is_empty() {
        println!("Repos:    {}", fragment.provides.repos.join(", "));
    }
    if !fragment.packages.required.is_empty() {
        println!("Packages: {}", fragment.packages.required.join(", "));
    }

    if !tree_paths.is_empty() {
        println!();
        println!("tree/");
        for p in &tree_paths {
            println!("  {}", p);
        }
    }

    print_mount_section(&mount_section);

    println!();
    if !hook_paths.is_empty() {
        println!("hooks/");
        for hook in &hook_paths {
            println!("  {}", hook);
        }
    } else {
        println!("hooks/ (none)");
    }

    Ok(())
}

/// Mode of `hooks/entrypoint` under `dir`, when it is a regular file.
///
/// `std::fs::metadata` follows symlinks, so a symlinked entrypoint resolving
/// to an executable regular file is accepted here. The registry path never
/// sees that case: it rejects links anywhere in a layer for unrelated safety
/// reasons, before this rule is evaluated.
fn local_entrypoint_mode(dir: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(dir.join("hooks").join(HOOKS_ENTRYPOINT_NAME)).ok()?;
    metadata.is_file().then(|| metadata.permissions().mode())
}

/// The `mount/` section for one fragment: the derived targets, the notice
/// for a `mount/` directory that holds no files, and the annotation drift
/// warning. Drift has no counterpart on the local-dir path, which reads no
/// registry annotation to drift against, so it is always `None` there.
struct MountSection {
    targets: Vec<String>,
    notice: Option<String>,
    drift: Option<String>,
}

/// Build the section from a local fragment directory, reaching no registry.
///
/// This is the pre-publish confirmation surface: run against the fragment
/// directory it shows the targets generation will derive, before the
/// fragment is published anywhere.
fn local_mount_section(dir: &Path, fragment_name: &str) -> Result<MountSection> {
    let mut files = Vec::new();
    collect_display_paths(dir, "mount", &mut files)?;
    let files: Vec<std::path::PathBuf> = files.iter().map(std::path::PathBuf::from).collect();

    let derived = derive_mount_points(fragment_name, &files)?;
    let notice = empty_mount_notice(fragment_name, dir.join("mount").is_dir(), &derived);
    Ok(MountSection {
        targets: derived.iter().map(MountPoint::target).collect(),
        notice,
        drift: None,
    })
}

/// Build the section for a registry-loaded fragment, from evidence already
/// carried on `LoadedFragment`. `load_registry_fragment` derives the mount
/// points and the drift warning at load time but prints neither: that
/// choice is caller-owned so the same load path serves `list`, which stays
/// silent, and generation, which prints from `validate_composition`
/// instead. This function reads only that carried evidence, does no I/O of
/// its own, and is therefore safe to unit test with a constructed fixture
/// rather than a real registry pull.
fn registry_mount_section(loaded: &LoadedFragment) -> MountSection {
    let notice = empty_mount_notice(
        loaded.fragment.name.as_str(),
        loaded.has_mount_dir,
        &loaded.mount_points,
    );
    MountSection {
        targets: loaded.mount_points.iter().map(MountPoint::target).collect(),
        notice,
        drift: loaded.drift_warning.clone(),
    }
}

/// Print the section, or nothing when the fragment carries no mounts and no
/// diagnostic to raise about them.
fn print_mount_section(section: &MountSection) {
    if let Some(notice) = &section.notice {
        eprintln!("{}", notice);
    }
    if let Some(drift) = &section.drift {
        eprintln!("{}", drift);
    }
    if section.targets.is_empty() {
        return;
    }
    println!();
    println!("mount/");
    for target in &section.targets {
        println!("  {}", target);
    }
    println!("{}", MOUNT_SECTION_NOTE);
}

fn collect_display_paths(base: &Path, prefix: &str, paths: &mut Vec<String>) -> Result<()> {
    let dir = base.join(prefix);
    if !dir.exists() {
        return Ok(());
    }
    collect_display_recursive(&dir, base, prefix, paths)?;
    paths.sort();
    Ok(())
}

fn collect_display_recursive(
    dir: &Path,
    base: &Path,
    prefix: &str,
    paths: &mut Vec<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_display_recursive(&path, base, prefix, paths)?;
        } else {
            let rel = path.strip_prefix(base)?;
            // Display paths are relative to the scanned subtree
            let display = rel
                .strip_prefix(prefix)
                .unwrap_or(rel)
                .to_string_lossy()
                .to_string();
            paths.push(display);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fragment directory carrying `fragment.toml` plus the given hook
    /// files, each written at the requested mode.
    fn fragment_dir(hooks: &[(&str, u32)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fragment.toml"),
            "[fragment]\nname = \"nvidia-driver\"\nversion = \"1.0\"\ndescription = \"test\"\n",
        )
        .unwrap();
        for (rel, mode) in hooks {
            let hook_path = dir.path().join("hooks").join(rel);
            std::fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
            std::fs::write(&hook_path, "#!/bin/sh\necho hook\n").unwrap();
            std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(*mode)).unwrap();
        }
        dir
    }

    #[test]
    fn local_hooks_without_entrypoint_are_rejected() {
        let dir = fragment_dir(&[("other.sh", 0o755)]);
        let err = run_inspect(dir.path().to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("nvidia-driver") && err.contains("no executable hooks/entrypoint"),
            "local inspect must raise the same error as the registry path, got: {err}"
        );
    }

    /// Pins the recursive scan: hook files count at any depth, so a fragment
    /// whose hooks/ holds only a nested helper still needs an entrypoint. A
    /// shallow scan would see zero hook files and pass it, diverging from the
    /// registry path with a green suite.
    #[test]
    fn local_nested_hook_file_still_requires_an_entrypoint() {
        let dir = fragment_dir(&[("lib/helper.sh", 0o644)]);
        let err = run_inspect(dir.path().to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no executable hooks/entrypoint"),
            "a nested hook file must still require an entrypoint, got: {err}"
        );
    }

    #[test]
    fn local_non_executable_entrypoint_is_rejected() {
        let dir = fragment_dir(&[("entrypoint", 0o644)]);
        let err = run_inspect(dir.path().to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("hooks/entrypoint is not executable") && err.contains("chmod +x"),
            "local inspect must raise the mode message, got: {err}"
        );
    }

    #[test]
    fn local_entrypoint_with_support_files_is_accepted() {
        let dir = fragment_dir(&[("entrypoint", 0o755), ("lib/helper.sh", 0o644)]);
        assert!(run_inspect(dir.path().to_str().unwrap()).is_ok());
    }

    const MOUNT_FRAGMENT_TOML: &str = r#"
[fragment]
name = "rhel-entitlement"
version = "1.0"
description = "RHEL entitlement certificates for the build"
"#;

    fn write_mount_fragment(dir: &Path) {
        std::fs::write(dir.join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();
        let target = dir.join("mount/etc/pki/entitlement");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("cert.pem"), b"cert").unwrap();
        std::fs::write(target.join("key.pem"), b"key").unwrap();
    }

    #[test]
    fn local_inspect_derives_the_targets_generation_will_use() {
        let tmp = tempfile::tempdir().unwrap();
        write_mount_fragment(tmp.path());

        let section = local_mount_section(tmp.path(), "rhel-entitlement")
            .expect("a fragment directory with mount/ content derives targets");
        assert_eq!(
            section.targets,
            vec!["/etc/pki/entitlement".to_string()],
            "two files in one directory derive one target"
        );
        assert!(section.notice.is_none());
        assert!(
            section.drift.is_none(),
            "the local-dir path reads no registry annotation to drift against"
        );
    }

    #[test]
    fn local_inspect_notices_an_empty_mount_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();
        std::fs::create_dir_all(tmp.path().join("mount")).unwrap();

        let section = local_mount_section(tmp.path(), "rhel-entitlement").unwrap();
        assert!(section.targets.is_empty());
        assert!(
            section.notice.is_some(),
            "an empty mount/ is almost always an authoring mistake"
        );
    }

    #[test]
    fn a_fragment_without_mount_has_no_section() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();

        let section = local_mount_section(tmp.path(), "rhel-entitlement").unwrap();
        assert!(section.targets.is_empty());
        assert!(section.notice.is_none());
    }

    /// Minimal `LoadedFragment` fixture for the registry-section tests: the
    /// registry branch of `run_inspect` builds one from a real pull, and
    /// this module cannot reach a registry, so tests build the evidence a
    /// load would carry instead and drive `registry_mount_section` directly.
    fn test_loaded_fragment(name: &str) -> LoadedFragment {
        LoadedFragment {
            fragment: crate::fragment::Fragment {
                name: crate::fragment::FragmentName::new(name).expect("valid fragment name"),
                version: "1.0".into(),
                description: "test".into(),
                vendor: None,
                provides: crate::fragment::FragmentProvides { repos: vec![] },
                packages: crate::fragment::FragmentPackages { required: vec![] },
                conflicts: crate::fragment::FragmentConflicts { fragments: vec![] },
            },
            tree_paths: vec![],
            hook_paths: vec![],
            source: crate::manifest::FragmentSource::Registry {
                image_ref: "test/placeholder:latest".to_string(),
            },
            resolved_digest: None,
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
            mount_points: vec![],
            has_mount_dir: false,
            drift_warning: None,
        }
    }

    /// Pins Carry A: `load_registry_fragment` no longer prints the
    /// empty-mount notice at load (that printing moved to
    /// `validate_composition` for generation), so the registry-inspect
    /// section must compute it itself from the carried evidence rather than
    /// leaving it `None`.
    #[test]
    fn registry_inspect_notices_an_empty_mount_directory() {
        let mut loaded = test_loaded_fragment("rhel-entitlement");
        loaded.has_mount_dir = true;

        let section = registry_mount_section(&loaded);
        assert!(section.targets.is_empty());
        assert!(
            section.notice.is_some(),
            "an empty mount/ is a notice on the registry path too"
        );
        assert!(section.drift.is_none());
    }

    /// Pins Carry B: the registry-inspect section surfaces
    /// `LoadedFragment.drift_warning`, the annotation-vs-layer disagreement
    /// carried by the loader, which only `inspect` and `validate_composition`
    /// print.
    #[test]
    fn registry_inspect_surfaces_drift_from_loaded_evidence() {
        let mut loaded = test_loaded_fragment("rhel-entitlement");
        loaded.has_mount_dir = true;
        loaded.mount_points = vec![MountPoint::from_target("/etc/rhsm").unwrap()];
        let annotated = vec![MountPoint::from_target("/etc/pki/entitlement").unwrap()];
        loaded.drift_warning = crate::mount::mount_annotation_drift(
            loaded.fragment.name.as_str(),
            &annotated,
            &loaded.mount_points,
        );

        let section = registry_mount_section(&loaded);
        assert_eq!(section.targets, vec!["/etc/rhsm".to_string()]);
        assert!(section.notice.is_none());
        assert_eq!(
            section.drift, loaded.drift_warning,
            "the section must surface the loader's carried warning verbatim"
        );
    }

    #[test]
    fn registry_inspect_section_is_quiet_when_evidence_carries_nothing() {
        let mut loaded = test_loaded_fragment("epel");
        loaded.mount_points = vec![MountPoint::from_target("/etc/rhsm").unwrap()];
        loaded.has_mount_dir = true;

        let section = registry_mount_section(&loaded);
        assert_eq!(section.targets, vec!["/etc/rhsm".to_string()]);
        assert!(section.notice.is_none(), "mount/ carries files, no notice");
        assert!(section.drift.is_none(), "no drift_warning was carried");
    }
}
