use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;

pub fn validate_composition(
    _manifest: &Manifest,
    fragments: &[LoadedFragment],
) -> Result<DeduplicationResult> {
    check_duplicate_names(fragments)?;
    check_conflicts(fragments)?;
    let dedup = check_repo_deduplication(fragments)?;
    Ok(dedup)
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

/// For each repo ID provided by multiple fragments, compare the actual
/// .repo file content. Identical definitions are deduplicated (first
/// provider wins). Conflicting definitions (same repo ID, different
/// content) fail the build with a clear error.
pub fn check_repo_deduplication(fragments: &[LoadedFragment]) -> Result<DeduplicationResult> {
    // Map repo ID -> list of (fragment name, repo file content hash)
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
                .push((&f.fragment.name, content_hash));
        }
    }

    let mut deduplicated_repos: HashMap<String, String> = HashMap::new();
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

            // Identical definitions — first provider wins
            let canonical = providers[0].0;
            let skipped: Vec<&str> = providers[1..].iter().map(|(n, _)| *n).collect();
            eprintln!(
                "note: repo '{}' deduplicated — using '{}', skipping '{}'",
                repo_id,
                canonical,
                skipped.join("', '")
            );
            deduplicated_repos.insert(repo_id.clone(), canonical.to_string());
        }
    }

    Ok(DeduplicationResult { deduplicated_repos })
}

#[derive(Debug, Clone)]
pub struct DeduplicationResult {
    /// Map of repo ID -> canonical provider fragment name.
    /// Only populated for repo IDs with multiple providers.
    /// The generator skips repo COPY steps for non-canonical providers.
    pub deduplicated_repos: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::*;
    use crate::manifest::FragmentSource;

    fn test_fragment(name: &str, repos: Vec<&str>, conflicts: Vec<&str>) -> LoadedFragment {
        LoadedFragment {
            fragment: crate::fragment::Fragment {
                name: name.to_string(),
                version: "1.0".into(),
                description: "test".into(),
                vendor: None,
                phase: FragmentPhase::Config,
                provides: FragmentProvides {
                    repos: repos.into_iter().map(String::from).collect(),
                },
                packages: FragmentPackages { available: vec![] },
                conflicts: FragmentConflicts {
                    fragments: conflicts.into_iter().map(String::from).collect(),
                },
            },
            tree_paths: vec![],
            has_configure_script: false,
            source: FragmentSource::Registry {
                image_ref: "test/placeholder:latest".to_string(),
            },
            resolved_digest: None,
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
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

    #[test]
    fn repo_dedup_same_id_deduplicates() {
        let frags = vec![
            test_fragment("epel-user1", vec!["epel"], vec![]),
            test_fragment("epel-user2", vec!["epel"], vec![]),
        ];
        let result = check_repo_deduplication(&frags).unwrap();
        assert!(result.deduplicated_repos.contains_key("epel"));
        assert_eq!(result.deduplicated_repos["epel"], "epel-user1");
    }

    #[test]
    fn duplicate_fragment_names_fail() {
        let frags = vec![
            test_fragment("myapp", vec![], vec![]),
            test_fragment("myapp", vec![], vec![]),
        ];
        assert!(check_duplicate_names(&frags).is_err());
    }
}
