use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;
use crate::mount::empty_mount_notice;

/// Generation-time mount diagnostics live here, ahead of the validation
/// checks below: `validate_composition` is the choke point the generate
/// path (both self-contained and normal) always runs, while `inspect` and
/// `list` never call it. That makes emission generation-only without
/// threading a "should I print" flag through the shared loader. (Task 7
/// adds this composition's sibling notice, "mounts but no package step",
/// alongside this one.)
pub fn validate_composition(_manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    for f in fragments {
        if let Some(notice) =
            empty_mount_notice(f.fragment.name.as_str(), f.has_mount_dir, &f.mount_points)
        {
            eprintln!("{notice}");
        }
    }

    check_duplicate_names(fragments)?;
    check_conflicts(fragments)?;
    check_repo_conflicts(fragments)?;
    Ok(())
}

pub fn check_duplicate_names(fragments: &[LoadedFragment]) -> Result<()> {
    let mut seen = HashSet::new();
    for f in fragments {
        if !seen.insert(&f.fragment.name) {
            bail!(
                "duplicate fragment name '{}' — each fragment must have a unique name",
                f.fragment.name
            );
        }
    }
    Ok(())
}

pub fn check_conflicts(fragments: &[LoadedFragment]) -> Result<()> {
    let names: HashSet<&str> = fragments.iter().map(|f| f.fragment.name.as_str()).collect();

    for f in fragments {
        for conflict_name in &f.fragment.conflicts.fragments {
            if names.contains(conflict_name.as_str()) {
                bail!(
                    "fragment '{}' declares a conflict with '{}', which is also in the manifest",
                    f.fragment.name,
                    conflict_name
                );
            }
        }
    }
    Ok(())
}

/// For each repo ID provided by more than one fragment, compare what those
/// fragments ship. Disagreement fails the build with a clear error. Agreement
/// is allowed through: every provider still emits its own COPY, the last one
/// wins, and the generated Containerfile's header comment names the
/// collision. Nothing is silently skipped.
///
/// The comparison is coarser than per-repo-ID: each fragment contributes a
/// single hash over its *entire* `repo_file_contents` map, and that one hash
/// is attributed to every repo ID it provides. Two fragments that agree on a
/// shared repo ID but differ in some other `.repo` file they also ship will
/// therefore be reported as conflicting on the shared ID. Pre-existing
/// behavior; the shipped examples do not trip it.
pub fn check_repo_conflicts(fragments: &[LoadedFragment]) -> Result<()> {
    // Map repo ID -> list of (fragment name, whole-map content hash)
    let mut repo_providers: HashMap<String, Vec<(&str, u64)>> = HashMap::new();

    for f in fragments {
        for repo_id in &f.fragment.provides.repos {
            // Hash actual .repo file contents (populated during loading
            // for both local and registry fragments).
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            let mut sorted_files: Vec<_> = f.repo_file_contents.iter().collect();
            sorted_files.sort_by_key(|(name, _)| *name);
            for (name, content) in &sorted_files {
                name.hash(&mut hasher);
                content.hash(&mut hasher);
            }
            let content_hash = hasher.finish();

            repo_providers
                .entry(repo_id.clone())
                .or_default()
                .push((f.fragment.name.as_str(), content_hash));
        }
    }

    for (repo_id, providers) in &repo_providers {
        if providers.len() > 1 {
            // Check for conflicting definitions (different content hashes)
            let first_hash = providers[0].1;
            for (name, hash) in &providers[1..] {
                if *hash != first_hash {
                    bail!(
                        "repo '{}' has conflicting definitions: fragment '{}' and fragment '{}' provide different .repo content for the same repo ID",
                        repo_id,
                        providers[0].0,
                        name
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::*;
    use crate::manifest::FragmentSource;

    fn test_fragment(name: &str, repos: Vec<&str>, conflicts: Vec<&str>) -> LoadedFragment {
        LoadedFragment {
            fragment: crate::fragment::Fragment {
                name: FragmentName::new(name).expect("test fragment name must be valid"),
                version: "1.0".into(),
                description: "test".into(),
                vendor: None,
                provides: FragmentProvides {
                    repos: repos.into_iter().map(String::from).collect(),
                },
                packages: FragmentPackages { required: vec![] },
                conflicts: FragmentConflicts {
                    fragments: conflicts.into_iter().map(String::from).collect(),
                },
            },
            tree_paths: vec![],
            hook_paths: vec![],
            source: FragmentSource::Registry {
                image_ref: "test/placeholder:latest".to_string(),
            },
            resolved_digest: None,
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
            mount_points: vec![],
            has_mount_dir: false,
        }
    }

    #[test]
    fn no_conflicts_passes() {
        let frags = vec![
            test_fragment("a", vec!["repo-a"], vec![]),
            test_fragment("b", vec!["repo-b"], vec![]),
        ];
        assert!(check_conflicts(&frags).is_ok());
    }

    #[test]
    fn declared_conflict_fails() {
        let frags = vec![
            test_fragment("a", vec![], vec!["b"]),
            test_fragment("b", vec![], vec![]),
        ];
        assert!(check_conflicts(&frags).is_err());
    }

    #[test]
    fn mutual_conflict_fails() {
        let frags = vec![
            test_fragment("a", vec![], vec!["b"]),
            test_fragment("b", vec![], vec!["a"]),
        ];
        assert!(check_conflicts(&frags).is_err());
    }

    /// Identical repo definitions are not an error and are not skipped:
    /// both providers emit a COPY and the header comment names the collision.
    #[test]
    fn repo_same_id_identical_content_passes() {
        let frags = vec![
            test_fragment("epel-user1", vec!["epel"], vec![]),
            test_fragment("epel-user2", vec!["epel"], vec![]),
        ];
        assert!(check_repo_conflicts(&frags).is_ok());
    }

    #[test]
    fn duplicate_fragment_names_fail() {
        let frags = vec![
            test_fragment("myapp", vec![], vec![]),
            test_fragment("myapp", vec![], vec![]),
        ];
        assert!(check_duplicate_names(&frags).is_err());
    }

    #[test]
    fn conflicting_repo_content_fails() {
        let mut frag_a = test_fragment("provider-a", vec!["shared-repo"], vec![]);
        frag_a.repo_file_contents.insert(
            "shared.repo".to_string(),
            "[shared]\nbaseurl=https://a.example.com/\n".to_string(),
        );
        let mut frag_b = test_fragment("provider-b", vec!["shared-repo"], vec![]);
        frag_b.repo_file_contents.insert(
            "shared.repo".to_string(),
            "[shared]\nbaseurl=https://b.example.com/\n".to_string(),
        );
        let result = check_repo_conflicts(&[frag_a, frag_b]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("conflicting definitions"),
            "expected 'conflicting definitions', got: {}",
            err
        );
    }

    /// The repos-phase content restriction went with the phase field: a
    /// fragment providing repos may also carry hooks and non-repo tree
    /// paths, and composition validation has nothing to say about content
    /// mix. Placement is the generator's concern, gated on paths.
    #[test]
    fn repos_provider_carrying_hooks_and_config_passes_validation() {
        let mut frag = test_fragment("mixed", vec!["mixed-repo"], vec![]);
        frag.tree_paths = vec![
            std::path::PathBuf::from("tree/etc/yum.repos.d/mixed.repo"),
            std::path::PathBuf::from("tree/usr/lib/sysctl.d/99-mixed.conf"),
        ];
        frag.hook_paths = vec![std::path::PathBuf::from("entrypoint")];
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![crate::manifest::ManifestFragment {
                image: "quay.io/test/mixed:1.0".into(),
                packages: vec![],
                mirror: None,
            }],
        };
        assert!(validate_composition(&manifest, &[frag]).is_ok());
    }

    /// A present-but-empty `mount/` is a notice, not a validation failure:
    /// the composition still passes. The notice itself is a stderr side
    /// effect that this test cannot capture, so the substantive check is
    /// that the carried evidence (`has_mount_dir` plus empty `mount_points`)
    /// is exactly what makes `empty_mount_notice` fire at the call site
    /// `validate_composition` uses.
    #[test]
    fn composition_with_an_empty_mount_directory_still_validates() {
        let mut frag = test_fragment("mount-only", vec![], vec![]);
        frag.has_mount_dir = true;
        frag.mount_points = vec![];
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![crate::manifest::ManifestFragment {
                image: "quay.io/test/mount-only:1.0".into(),
                packages: vec![],
                mirror: None,
            }],
        };
        assert!(validate_composition(&manifest, &[frag.clone()]).is_ok());
        assert!(
            empty_mount_notice(
                frag.fragment.name.as_str(),
                frag.has_mount_dir,
                &frag.mount_points
            )
            .is_some(),
            "the evidence validate_composition reads must actually yield a notice"
        );
    }
}
