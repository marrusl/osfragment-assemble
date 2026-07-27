use anyhow::Result;
use std::path::Path;

use crate::fragment::parse_fragment_toml;

pub fn run_inspect(target: &str) -> Result<()> {
    let path = Path::new(target);

    let (fragment, tree_paths, script_paths) = if path.is_dir() {
        let toml_path = path.join("fragment.toml");
        let content = std::fs::read_to_string(&toml_path)?;
        let frag = parse_fragment_toml(&content)?;

        let mut paths = Vec::new();
        collect_display_paths(path, "tree", &mut paths)?;

        // Collect all .sh and .bash scripts in scripts/ directory
        let scripts_dir = path.join("scripts");
        let mut script_list = Vec::new();
        if scripts_dir.exists() && scripts_dir.is_dir() {
            for entry in std::fs::read_dir(&scripts_dir)? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_file() {
                    if let Some(name) = entry_path.file_name() {
                        let name_str = name.to_string_lossy();
                        if name_str.ends_with(".sh") || name_str.ends_with(".bash") {
                            script_list.push(name.to_os_string().into_string().unwrap());
                        }
                    }
                }
            }
            script_list.sort();
        }

        (frag, paths, script_list)
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
        let script_list: Vec<String> = loaded
            .script_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        (loaded.fragment, display_paths, script_list)
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
    if !script_paths.is_empty() {
        println!("scripts/");
        for script in &script_paths {
            println!("  {}", script);
        }
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
