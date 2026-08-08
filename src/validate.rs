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
/// alongside this one.) The annotation drift warning is carried on
/// `LoadedFragment.drift_warning` by the loader for the same reason and
/// printed here too, so the two generation-time mount diagnostics stay
/// co-located.
pub fn validate_composition(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    for f in fragments {
        if let Some(notice) =
            empty_mount_notice(f.fragment.name.as_str(), f.has_mount_dir, &f.mount_points)
        {
            eprintln!("{notice}");
        }
        if let Some(warning) = &f.drift_warning {
            eprintln!("{warning}");
        }
    }

    check_duplicate_names(fragments)?;
    check_conflicts(fragments)?;
    check_repo_conflicts(fragments)?;
    check_mount_digest_pins(manifest, fragments)?;
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

/// A build-mount fragment referenced without a digest is a generation error.
///
/// A movable tag on an artifact that injects trust material into the package
/// step is an invisible substitution point: whoever can move the tag can
/// swap a CA bundle or a credential and redirect the entire package fetch.
/// The pin is checked against the manifest's own image reference, so it
/// survives regardless of `--pin-digests` and needs no per-fragment
/// retention machinery.
pub fn check_mount_digest_pins(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    check_mount_digest_pins_with(manifest, fragments, |declared| {
        crate::loader::resolve_digest(declared).ok()
    })
}

/// [`check_mount_digest_pins`] with the digest resolver injected.
///
/// The resolver only runs on a path that is already about to abort — it
/// exists to enrich the error, not to decide whether one is raised — so a
/// test can drive a forced lookup failure through the real policy logic
/// below without reaching the network, and confirm that failure still
/// yields the same unpinned-reference error rather than a different one.
fn check_mount_digest_pins_with(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
    resolve: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    for f in fragments {
        if f.mount_points.is_empty() {
            continue;
        }
        let declared = &manifest.fragments[f.manifest_index].image;
        if declared.contains("@sha256:") {
            continue;
        }
        // The digest is already in hand under --pin-digests. Without it,
        // load_all_fragments dropped the one it resolved, so read it again
        // rather than printing a placeholder for something the tool can go
        // get. This runs only on a path that is about to abort, so one
        // registry read buys a fix the user can paste.
        let resolved = f.resolved_digest.clone().or_else(|| resolve(declared));
        bail!(
            "{}",
            unpinned_mount_error(f.fragment.name.as_str(), declared, resolved.as_deref())
        );
    }
    Ok(())
}

/// The unpinned build-mount error text.
///
/// `resolved` is the digest the tool read for `declared`, when it could read
/// one. Present, the corrected `image:` line is complete and the fix is a
/// paste. Absent, the line keeps its placeholder and the skopeo command that
/// fills it in follows, because that is the only case where the user has a
/// lookup left to do.
fn unpinned_mount_error(name: &str, declared: &str, resolved: Option<&str>) -> String {
    let (repository, _tag) = crate::generator::split_image_ref(declared);
    let (corrected, guidance) = match resolved {
        Some(digest) => (format!("{}@{}", repository, digest), String::new()),
        None => (
            format!("{}@sha256:...", repository),
            format!(
                "\nObtain the digest with:\n\
                 \x20   skopeo inspect --format '{{{{.Digest}}}}' docker://{}",
                declared
            ),
        ),
    };
    format!(
        "fragment '{}' carries build mounts but its manifest entry is not pinned to a \
         digest: {}. A movable tag on an artifact that injects trust material into the \
         package step is an invisible substitution point: whoever can move the tag can \
         swap a credential or a CA bundle and redirect the whole package fetch. Pin it \
         by digest in the manifest:\n\
         \x20   image: {}{}",
        name, declared, corrected, guidance
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::*;
    use crate::manifest::FragmentSource;
    use crate::manifest::{Manifest, ManifestFragment};
    use std::path::PathBuf;

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
            drift_warning: None,
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

    /// A fragment carrying drift evidence still validates: the warning is a
    /// stderr side effect this test cannot capture, so the substantive check
    /// is that carrying `Some(drift_warning)` on `LoadedFragment` neither
    /// fails validation nor gets dropped along the way to the call site
    /// `validate_composition` prints it from.
    #[test]
    fn composition_with_drift_evidence_still_validates() {
        let mut frag = test_fragment("drifted", vec![], vec![]);
        frag.has_mount_dir = true;
        frag.mount_points = vec![crate::mount::MountPoint::from_target("/etc/rhsm").unwrap()];
        let annotated =
            vec![crate::mount::MountPoint::from_target("/etc/pki/entitlement").unwrap()];
        frag.drift_warning = crate::mount::mount_annotation_drift(
            frag.fragment.name.as_str(),
            &annotated,
            &frag.mount_points,
        );
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![crate::manifest::ManifestFragment {
                // Pinned: this test carries mount_points, and an unpinned
                // build-mount reference is now its own generation error
                // (check_mount_digest_pins). Unrelated to what this test
                // checks, so the pin just needs to be present.
                image: "quay.io/test/drifted@sha256:abc123".into(),
                packages: vec![],
                mirror: None,
            }],
        };
        assert!(validate_composition(&manifest, &[frag.clone()]).is_ok());
        assert!(
            frag.drift_warning.is_some(),
            "the evidence validate_composition reads must actually carry a warning"
        );
    }

    /// A digest already in hand at validation time, as `--pin-digests` leaves
    /// it. Holding one keeps these tests off the network: the error path
    /// falls back to a live `resolve_digest` only when this is `None`, and
    /// `Option::or_else` is lazy.
    const TEST_MOUNT_DIGEST: &str = "sha256:abc123";

    fn mount_fragment(name: &str, image: &str) -> (LoadedFragment, ManifestFragment) {
        let mut loaded = test_fragment(name, vec![], vec![]);
        loaded.source = FragmentSource::Registry {
            image_ref: image.to_string(),
        };
        loaded.resolved_digest = Some(TEST_MOUNT_DIGEST.to_string());
        loaded.mount_points = crate::mount::derive_mount_points(
            name,
            &[PathBuf::from("etc/pki/entitlement/cert.pem")],
        )
        .expect("fixture derives one mount point");
        (
            loaded,
            ManifestFragment {
                image: image.to_string(),
                packages: vec!["some-package".into()],
                mirror: None,
            },
        )
    }

    fn manifest_of(entries: Vec<ManifestFragment>) -> Manifest {
        Manifest {
            base: "quay.io/test/base:1".into(),
            fragments: entries,
            source_path: "test-manifest.yaml".into(),
        }
    }

    #[test]
    fn an_unpinned_build_mount_reference_is_a_generation_error() {
        let (loaded, mf) = mount_fragment("rhel-entitlement", "quay.io/acme/rhel-entitlement:1.0");
        let manifest = manifest_of(vec![mf]);
        let err = check_mount_digest_pins(&manifest, &[loaded])
            .expect_err("a movable tag on mount material is an invisible substitution point")
            .to_string();

        assert!(
            err.contains("rhel-entitlement"),
            "names the fragment: {err}"
        );
        assert!(
            err.contains("image: quay.io/acme/rhel-entitlement@sha256:abc123"),
            "prints the corrected image: line with the resolved digest filled in: {err}"
        );
        assert!(
            !err.contains("@sha256:..."),
            "no placeholder to fill in when the tool is holding the digest: {err}"
        );
    }

    #[test]
    fn the_unpinned_error_keeps_the_placeholder_when_no_digest_resolved() {
        // The only case where the user has a lookup left to do, so this is
        // the one branch that carries the skopeo command.
        let err = unpinned_mount_error(
            "rhel-entitlement",
            "quay.io/acme/rhel-entitlement:1.0",
            None,
        );
        assert!(
            err.contains("image: quay.io/acme/rhel-entitlement@sha256:..."),
            "got: {err}"
        );
        assert!(
            err.contains("skopeo inspect"),
            "shows how to obtain a digest: {err}"
        );
    }

    /// Drives a failing digest lookup through the real validation path
    /// rather than just the message builder: an injected resolver that
    /// always fails stands in for a `resolve_digest` that errored (network
    /// down, bad ref, whatever). The contract that matters is that this
    /// still surfaces the same unpinned-reference error with its placeholder
    /// and skopeo guidance, never a distinct "lookup failed" error — a
    /// regression at the `.ok()` conversion from `resolve_digest`'s `Result`
    /// would otherwise go uncaught.
    #[test]
    fn a_failing_resolver_still_yields_the_unpinned_placeholder_error() {
        let (mut loaded, mf) =
            mount_fragment("rhel-entitlement", "quay.io/acme/rhel-entitlement:1.0");
        loaded.resolved_digest = None; // force the fallback to the injected resolver
        let manifest = manifest_of(vec![mf]);

        let err = check_mount_digest_pins_with(&manifest, &[loaded], |_| None)
            .expect_err("an unpinned reference is still an error when digest resolution fails")
            .to_string();

        assert!(
            err.contains("image: quay.io/acme/rhel-entitlement@sha256:..."),
            "keeps the placeholder when resolution failed: {err}"
        );
        assert!(
            err.contains("skopeo inspect"),
            "shows how to obtain a digest: {err}"
        );
        assert!(
            !err.contains("lookup failed"),
            "must not leak resolve_digest's own error text as a different error: {err}"
        );
    }

    /// Symmetric positive: an injected resolver that succeeds fills in the
    /// digest it found, exercised through the same real validation path.
    #[test]
    fn a_succeeding_resolver_fills_in_the_digest_it_finds() {
        let (mut loaded, mf) =
            mount_fragment("rhel-entitlement", "quay.io/acme/rhel-entitlement:1.0");
        loaded.resolved_digest = None; // force the fallback to the injected resolver
        let manifest = manifest_of(vec![mf]);

        let err = check_mount_digest_pins_with(&manifest, &[loaded], |_| {
            Some("sha256:def456".to_string())
        })
        .expect_err("still unpinned in the manifest regardless of what the resolver found")
        .to_string();

        assert!(
            err.contains("image: quay.io/acme/rhel-entitlement@sha256:def456"),
            "prints the digest the injected resolver returned: {err}"
        );
    }

    #[test]
    fn a_pinned_build_mount_reference_passes() {
        let (loaded, mf) = mount_fragment(
            "rhel-entitlement",
            "quay.io/acme/rhel-entitlement@sha256:abc123",
        );
        let manifest = manifest_of(vec![mf]);
        assert!(check_mount_digest_pins(&manifest, &[loaded]).is_ok());
    }

    #[test]
    fn a_fragment_without_mounts_needs_no_pin() {
        // The one deliberate asymmetry: ordinary fragments pin only under
        // --pin-digests, and this check must not quietly extend to them.
        let loaded = test_fragment("epel", vec!["epel"], vec![]);
        let manifest = manifest_of(vec![ManifestFragment {
            image: "quay.io/acme/epel:10".into(),
            packages: vec![],
            mirror: None,
        }]);
        assert!(check_mount_digest_pins(&manifest, &[loaded]).is_ok());
    }
}
