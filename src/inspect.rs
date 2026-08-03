use anyhow::Result;
use std::path::Path;

use crate::fragment::parse_fragment_toml;
use crate::loader::{validate_hooks_entrypoint, HOOKS_ENTRYPOINT_NAME};

pub fn run_inspect(target: &str) -> Result<()> {
    let path = Path::new(target);

    let (fragment, tree_paths, hook_paths) = if path.is_dir() {
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
            validate_hooks_entrypoint(&frag.name, local_entrypoint_mode(path))?;
        }

        (frag, paths, hook_list)
    } else {
        // Inspect requires tree/ contents and hooks — always do a
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
        (loaded.fragment, display_paths, hook_list)
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
}
