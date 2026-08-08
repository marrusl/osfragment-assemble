use anyhow::Result;

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;
use crate::mount::{MountPoint, MOUNT_SECTION_NOTE};

/// The continuation line under a fragment's table row, naming what it mounts
/// into the package step. `None` when the fragment mounts nothing.
///
/// A line rather than a column: adding a column would rewrite the table for
/// every fragment, and most fragments carry no mounts at all.
fn mount_line(points: &[MountPoint]) -> Option<String> {
    if points.is_empty() {
        return None;
    }
    Some(format!(
        "      mounts: {}",
        points
            .iter()
            .map(MountPoint::target)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

pub fn run_list(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    println!("Manifest: {}", manifest.source_path);
    println!("Base:     {}", manifest.base);
    println!();

    let has_digests = fragments.iter().any(|f| f.resolved_digest.is_some());

    if has_digests {
        println!(
            "  {:<20} {:<10} {:<20} PACKAGES",
            "NAME", "VERSION", "DIGEST"
        );
    } else {
        println!("  {:<20} {:<10} PACKAGES", "NAME", "VERSION");
    }

    for loaded in fragments {
        let mf = &manifest.fragments[loaded.manifest_index];
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
                "  {:<20} {:<10} {:<20} {}",
                loaded.fragment.name, loaded.fragment.version, digest_short, packages
            );
        } else {
            println!(
                "  {:<20} {:<10} {}",
                loaded.fragment.name, loaded.fragment.version, packages
            );
        }
        if let Some(line) = mount_line(&loaded.mount_points) {
            println!("{}", line);
        }
    }

    println!();
    println!("{} fragments", fragments.len());

    // Only when something in the manifest mounts: the note explains a line
    // the reader just saw, and means nothing without it. Reading these from
    // the mounts annotation is what lets this run without pulling layers.
    if fragments.iter().any(|f| !f.mount_points.is_empty()) {
        println!("{}", MOUNT_SECTION_NOTE);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mount_points(files: &[&str]) -> Vec<crate::mount::MountPoint> {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        crate::mount::derive_mount_points("test", &files).expect("fixture derives")
    }

    #[test]
    fn the_mount_line_lists_every_target_for_one_fragment() {
        let points = mount_points(&["etc/pki/entitlement/c.pem", "etc/rhsm/rhsm.conf"]);
        assert_eq!(
            mount_line(&points).as_deref(),
            Some("      mounts: /etc/pki/entitlement, /etc/rhsm")
        );
    }

    #[test]
    fn a_fragment_without_mounts_gets_no_line() {
        assert!(mount_line(&[]).is_none());
    }
}
