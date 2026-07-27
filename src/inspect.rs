use anyhow::Result;
use std::path::Path;

use crate::fragment::parse_fragment_toml;

pub fn run_inspect(target: &str) -> Result<()> {
    let path = Path::new(target);

    let (fragment, tree_paths, has_script) = if path.is_dir() {
        let toml_path = path.join("fragment.toml");
        let content = std::fs::read_to_string(&toml_path)?;
        let frag = parse_fragment_toml(&content)?;

        let mut paths = Vec::new();
        collect_display_paths(path, "tree", &mut paths)?;
        let script_paths_exist = path.join("scripts/configure.sh").exists();

        (frag, paths, script_paths_exist)
    } else {
        // Inspect requires tree/ contents and scripts — always do a
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
        (loaded.fragment, display_paths, loaded.has_configure_script)
    };

    let phase_str = match fragment.phase {
        crate::fragment::FragmentPhase::Repos => "repos",
        crate::fragment::FragmentPhase::Config => "config",
    };

    println!("Fragment: {} v{}", fragment.name, fragment.version);
    if let Some(v) = &fragment.vendor {
        println!("Vendor:   {}", v);
    }
    println!("Phase:    {}", phase_str);
    if !fragment.provides.repos.is_empty() {
        println!("Repos:    {}", fragment.provides.repos.join(", "));
    }
    if !fragment.packages.available.is_empty() {
        println!("Packages: {}", fragment.packages.available.join(", "));
    }

    if !tree_paths.is_empty() {
        println!();
        println!("tree/");
        for p in &tree_paths {
            println!("  {}", p);
        }
    }

    println!();
    if has_script {
        println!("scripts/");
        println!("  configure.sh (present)");
    } else {
        println!("scripts/ (none)");
    }

    Ok(())
}

fn collect_display_paths(base: &Path, prefix: &str, paths: &mut Vec<String>) -> Result<()> {
    let dir = base.join(prefix);
    if !dir.exists() {
        return Ok(());
    }
    collect_display_recursive(&dir, base, paths)?;
    paths.sort();
    Ok(())
}

fn collect_display_recursive(dir: &Path, base: &Path, paths: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_display_recursive(&path, base, paths)?;
        } else {
            let rel = path.strip_prefix(base)?;
            // Strip the "tree/" prefix for display
            let display = rel
                .strip_prefix("tree/")
                .unwrap_or(rel)
                .to_string_lossy()
                .to_string();
            paths.push(display);
        }
    }
    Ok(())
}
