use anyhow::Result;

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;

pub fn run_list(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    println!("Manifest: bootc-assemble.yaml");
    println!("Base:     {}", manifest.base);
    println!();

    let has_digests = fragments.iter().any(|f| f.resolved_digest.is_some());

    if has_digests {
        println!(
            "  {:<20} {:<8} {:<10} {:<20} PACKAGES",
            "NAME", "PHASE", "VERSION", "DIGEST"
        );
    } else {
        println!(
            "  {:<20} {:<8} {:<10} PACKAGES",
            "NAME", "PHASE", "VERSION"
        );
    }

    for loaded in fragments {
        let mf = &manifest.fragments[loaded.manifest_index];
        let phase_str = match loaded.fragment.phase {
            crate::fragment::FragmentPhase::Repos => "repos",
            crate::fragment::FragmentPhase::Config => "config",
        };
        let packages = if mf.packages.is_empty() {
            "\u{2014}".to_string()
        } else {
            mf.packages.join(", ")
        };
        if has_digests {
            let digest_short = loaded
                .resolved_digest
                .as_deref()
                .map(|d| {
                    let hash = d.strip_prefix("sha256:").unwrap_or(d);
                    format!("sha256:{}...", &hash[..12.min(hash.len())])
                })
                .unwrap_or_else(|| "(local)".to_string());
            println!(
                "  {:<20} {:<8} {:<10} {:<20} {}",
                loaded.fragment.name, phase_str, loaded.fragment.version, digest_short, packages
            );
        } else {
            println!(
                "  {:<20} {:<8} {:<10} {}",
                loaded.fragment.name, phase_str, loaded.fragment.version, packages
            );
        }
    }

    println!();
    println!("{} fragments", fragments.len());

    Ok(())
}
