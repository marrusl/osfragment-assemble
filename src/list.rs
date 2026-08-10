use anyhow::Result;

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;
use crate::mount::{MountPoint, MOUNT_SECTION_NOTE};

/// The continuation line under a fragment's table row, naming what it mounts
/// into the package and hook steps. `None` when the fragment mounts nothing.
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

/// The digest column's short form: the resolved digest truncated to its
/// first 12 hex characters, or `(local)` for a fragment that has none.
fn digest_short(resolved_digest: Option<&str>) -> String {
    resolved_digest
        .map(|d| {
            let hash = d.strip_prefix("sha256:").unwrap_or(d);
            format!("sha256:{}...", &hash[..12.min(hash.len())])
        })
        .unwrap_or_else(|| "(local)".to_string())
}

/// The lines one fragment contributes to the listing: its table row, wide
/// when the listing carries digests and narrow when it does not, then its
/// mount line when it has mounts.
///
/// Both row forms are built here so the mount line can be appended once,
/// after whichever was chosen. Printing it from inside the `has_digests`
/// branches instead would put it on only one of them, and `run_list` prints
/// straight to stdout with no offline surface to catch that.
fn fragment_lines(loaded: &LoadedFragment, packages: &str, has_digests: bool) -> Vec<String> {
    let row = if has_digests {
        format!(
            "  {:<20} {:<10} {:<20} {}",
            loaded.fragment.name,
            loaded.fragment.version,
            digest_short(loaded.resolved_digest.as_deref()),
            packages
        )
    } else {
        format!(
            "  {:<20} {:<10} {}",
            loaded.fragment.name, loaded.fragment.version, packages
        )
    };
    let mut lines = vec![row];
    lines.extend(mount_line(&loaded.mount_points));
    lines
}

/// The note the listing closes with, or `None` when nothing in it mounts:
/// the note explains a line the reader just saw, and means nothing without
/// one. Reading the mounts from the annotation is what lets this run without
/// pulling layers.
///
/// Takes the whole listing and returns a single value, so the note cannot
/// become per-fragment, and the gate is assertable without printing.
fn note_line(fragments: &[LoadedFragment]) -> Option<&'static str> {
    fragments
        .iter()
        .any(|f| !f.mount_points.is_empty())
        .then_some(MOUNT_SECTION_NOTE)
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
        for line in fragment_lines(loaded, &packages, has_digests) {
            println!("{}", line);
        }
    }

    println!();
    println!("{} fragments", fragments.len());

    if let Some(note) = note_line(fragments) {
        println!("{}", note);
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

    /// A listing fixture: `run_list` needs a registry, so the helpers it
    /// calls are the only offline surface, and they read nothing from a
    /// fragment beyond these fields.
    fn listed_fragment(name: &str, digest: Option<&str>, mounts: &[&str]) -> LoadedFragment {
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
            resolved_digest: digest.map(str::to_string),
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
            mount_points: mount_points(mounts),
            has_mount_dir: !mounts.is_empty(),
            drift_warning: None,
        }
    }

    /// The mount line belongs to the fragment, not to one of the two table
    /// widths, so it must follow the row in both. A listing mixing pinned
    /// and unpinned fragments renders every row in the wide form, so a mount
    /// line emitted from inside one digest branch would go missing for half
    /// a real listing while the other half kept it.
    #[test]
    fn the_mount_line_follows_the_row_in_both_table_widths() {
        let mounting = listed_fragment("entitlement", None, &["etc/rhsm/rhsm.conf"]);

        for has_digests in [true, false] {
            let lines = fragment_lines(&mounting, "vim", has_digests);
            assert_eq!(
                lines.len(),
                2,
                "row then mount line, has_digests={has_digests}"
            );
            assert!(
                lines[0].contains("entitlement"),
                "the row comes first: {lines:?}"
            );
            assert_eq!(
                lines[1], "      mounts: /etc/rhsm",
                "the mount line comes second, has_digests={has_digests}"
            );
        }
    }

    #[test]
    fn a_mountless_fragment_contributes_only_its_row_in_both_table_widths() {
        let plain = listed_fragment("epel", Some("sha256:abcdef0123456789"), &[]);

        for has_digests in [true, false] {
            let lines = fragment_lines(&plain, "vim", has_digests);
            assert_eq!(lines.len(), 1, "no mount line, has_digests={has_digests}");
        }
    }

    #[test]
    fn the_digest_column_appears_only_in_the_wide_row() {
        let pinned = listed_fragment("epel", Some("sha256:abcdef0123456789aaaa"), &[]);

        assert!(fragment_lines(&pinned, "vim", true)[0].contains("sha256:abcdef012345..."));
        assert!(!fragment_lines(&pinned, "vim", false)[0].contains("sha256:"));
        assert!(
            fragment_lines(&listed_fragment("local", None, &[]), "vim", true)[0]
                .contains("(local)"),
            "an unpinned fragment in a pinned listing still fills the column"
        );
    }

    /// The note is one line for the run, gated on the listing as a whole.
    /// Dropping the gate would print it for compositions that mount nothing,
    /// where it explains a line the reader never saw.
    #[test]
    fn the_closing_note_fires_once_for_a_listing_that_mounts_anything() {
        let mounting = listed_fragment("entitlement", None, &["etc/rhsm/rhsm.conf"]);
        let plain = listed_fragment("epel", None, &[]);

        assert_eq!(note_line(&[]), None, "an empty listing");
        assert_eq!(
            note_line(&[plain.clone(), plain.clone()]),
            None,
            "nothing in the listing mounts"
        );
        assert_eq!(
            note_line(&[plain.clone(), mounting.clone(), plain]),
            Some(MOUNT_SECTION_NOTE),
            "one mounting fragment among several is enough"
        );
        assert_eq!(
            note_line(&[mounting.clone(), mounting]),
            Some(MOUNT_SECTION_NOTE),
            "and two of them still produce the one note"
        );
    }
}
