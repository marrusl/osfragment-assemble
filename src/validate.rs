use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

use crate::loader::LoadedFragment;
use crate::manifest::Manifest;
use crate::mount::empty_mount_notice;
use crate::mount::GENERATOR_WRITTEN_PATHS;

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
    check_mount_overlaps(fragments)?;
    if let Some(notice) = unattached_mount_notice(manifest, fragments) {
        eprintln!("{}", notice);
    }
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
    check_mount_digest_pins_with(manifest, fragments, crate::loader::resolve_digest)
}

/// [`check_mount_digest_pins`] with the digest resolver injected.
///
/// `resolve` mirrors [`crate::loader::resolve_digest`]'s own signature
/// rather than an already-swallowed `Option`, so the point where a resolver
/// failure gets folded into "no resolved digest available" sits inside this
/// function, where a test can drive it, instead of at the call site above.
/// This is the exact place the unpinned-reference contract lives: a
/// resolver failure must enrich the error and never become a different one.
fn check_mount_digest_pins_with(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
    resolve: impl Fn(&str) -> Result<String>,
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
        // registry read buys a fix the user can paste. A resolver failure
        // here is swallowed into "no digest available": message
        // enrichment, not a new failure mode, so the unpinned-reference
        // error below is what results either way.
        let resolved = f.resolved_digest.clone().or_else(|| resolve(declared).ok());
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

/// Overlap between mount targets is prefix-based: two targets collide when
/// either equals or is an ancestor of the other, because a bind mount hides
/// whatever its target directory already contained.
///
/// Both directions are refused. First-wins on credentials produces silent
/// authentication mysteries, so the tool refuses instead.
pub fn check_mount_overlaps(fragments: &[LoadedFragment]) -> Result<()> {
    for (i, f) in fragments.iter().enumerate() {
        for point in &f.mount_points {
            // Against the paths the generator itself writes. Unconditional
            // rather than conditioned on some fragment shipping repo files:
            // the base image's own repo definitions sit at the same paths,
            // and a rule that depends on which other fragments happen to be
            // composed is a rule that fires unpredictably.
            for written in GENERATOR_WRITTEN_PATHS {
                if point.shadows(written.path) {
                    bail!(
                        "fragment '{}' mounts build material at {}, which equals or contains \
                         {}, where the generator's {} phase writes ahead of the package step. \
                         A bind mount hides whatever its target directory already contained, \
                         so this would hide that material during exactly the RUN that needs \
                         it. Move the material under a path that does not contain {}, for \
                         example mount/etc/pki/entitlement.",
                        f.fragment.name,
                        point.target(),
                        written.path,
                        written.phase,
                        written.path
                    );
                }
            }

            // Against every later fragment's targets. Comparing forward only
            // covers each pair once, and overlaps is symmetric.
            for other in &fragments[i + 1..] {
                for other_point in &other.mount_points {
                    if point.overlaps(other_point) {
                        bail!(
                            "fragments '{}' and '{}' mount build material at colliding paths: \
                             {} and {}. Two mount targets collide when either equals or is an \
                             ancestor of the other, because the inner mount is hidden by the \
                             outer one for the whole package step. First wins on credentials \
                             produces silent authentication mysteries, so this is refused \
                             rather than resolved. Change one fragment's mount/ subtree so \
                             the targets are unrelated paths, or compose only one of them.",
                            f.fragment.name,
                            other.fragment.name,
                            point.target(),
                            other_point.target()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Notice for a composition that carries build mounts and installs no
/// packages.
///
/// Build mounts attach to the batched dnf RUN, and that RUN is emitted only
/// when something is being installed. With nothing to install there is
/// nothing to attach to, and the mounts are silently absent from the output.
/// Reported rather than refused: the composition is well formed, and the
/// missing piece is a package selection the user still has to make.
pub fn unattached_mount_notice(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
) -> Option<String> {
    let mounting: Vec<&str> = fragments
        .iter()
        .filter(|f| !f.mount_points.is_empty())
        .map(|f| f.fragment.name.as_str())
        .collect();
    if mounting.is_empty() {
        return None;
    }

    let installs_anything = fragments
        .iter()
        .any(|f| !f.fragment.packages.required.is_empty())
        || manifest.fragments.iter().any(|mf| !mf.packages.is_empty());
    if installs_anything {
        return None;
    }

    Some(format!(
        "notice: {} carries build mounts, but this composition installs no packages, so \
         there is no dnf step for them to attach to and no mount is emitted. Select \
         packages on a fragment entry in the manifest, or publish the fragment with \
         packages.required set.",
        mounting
            .iter()
            .map(|n| format!("fragment '{}'", n))
            .collect::<Vec<_>>()
            .join(", ")
    ))
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

    /// Drives an actual resolver *failure* (an `Err`, matching what
    /// `resolve_digest` itself returns on a real lookup failure) through the
    /// real validation path, not just the message builder. The contract
    /// that matters is that `check_mount_digest_pins_with`'s own
    /// `Err`-to-"no digest" swallowing still surfaces the unpinned-reference
    /// error with its placeholder and skopeo guidance, and never leaks the
    /// resolver's own failure text as a different error. Because the
    /// injected closure returns `Result<String>` (the same type
    /// `resolve_digest` returns), this exercises the real `.ok()`
    /// conversion inside `check_mount_digest_pins_with` rather than
    /// sidestepping it with a pre-swallowed `Option`.
    #[test]
    fn a_failing_resolver_still_yields_the_unpinned_placeholder_error() {
        let (mut loaded, mf) =
            mount_fragment("rhel-entitlement", "quay.io/acme/rhel-entitlement:1.0");
        loaded.resolved_digest = None; // force the fallback to the injected resolver
        let manifest = manifest_of(vec![mf]);

        let err = check_mount_digest_pins_with(&manifest, &[loaded], |image_ref| {
            Err(anyhow::anyhow!(
                "skopeo digest lookup failed for {}: exit status 1",
                image_ref
            ))
        })
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
            !err.contains("skopeo digest lookup failed"),
            "must not leak the resolver's own failure text as a different error: {err}"
        );
    }

    /// Symmetric positive: an injected resolver that succeeds (`Ok`, again
    /// matching `resolve_digest`'s real return type) fills in the digest it
    /// found, exercised through the same real validation path.
    #[test]
    fn a_succeeding_resolver_fills_in_the_digest_it_finds() {
        let (mut loaded, mf) =
            mount_fragment("rhel-entitlement", "quay.io/acme/rhel-entitlement:1.0");
        loaded.resolved_digest = None; // force the fallback to the injected resolver
        let manifest = manifest_of(vec![mf]);

        let err =
            check_mount_digest_pins_with(&manifest, &[loaded], |_| Ok("sha256:def456".to_string()))
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

    fn mount_fragment_at(name: &str, mount_files: &[&str]) -> LoadedFragment {
        let mut loaded = test_fragment(name, vec![], vec![]);
        let files: Vec<PathBuf> = mount_files.iter().map(PathBuf::from).collect();
        loaded.mount_points =
            crate::mount::derive_mount_points(name, &files).expect("fixture derives");
        loaded
    }

    #[test]
    fn two_fragments_mounting_colliding_targets_is_an_error() {
        // (first fragment's files, second fragment's files, collides)
        //
        // Brief deviation: the ancestor and prefix-lookalike cases originally
        // used a bare "etc/pki/a.pem" for the first fragment, deriving target
        // /etc/pki. That path is itself an ancestor of the generator-written
        // /etc/pki/rpm-gpg (Task 1's GENERATOR_WRITTEN_PATHS), so it trips
        // the generator-written-path check on the first fragment alone,
        // before the two-fragment comparison this test exercises ever runs.
        // Deepened to "etc/pki/tls/..." to keep each case's intent (ancestor
        // collision; textual-prefix non-collision) without touching a
        // generator-reserved path.
        // The 4th element names the target path(s) the error must contain
        // when the case collides, locking the spec's "collision errors name
        // the shared/overlapping target" contract in place. Empty for the
        // non-colliding cases, where there is no error to check.
        type CollisionCase = (
            &'static [&'static str],
            &'static [&'static str],
            bool,
            &'static [&'static str],
        );
        let cases: &[CollisionCase] = &[
            // Identical targets.
            (
                &["etc/pki/entitlement/a.pem"],
                &["etc/pki/entitlement/b.pem"],
                true,
                &["/etc/pki/entitlement"],
            ),
            // One target is an ancestor of the other.
            (
                &["etc/pki/tls/a.pem"],
                &["etc/pki/tls/mirror/b.pem"],
                true,
                &["/etc/pki/tls", "/etc/pki/tls/mirror"],
            ),
            // Unrelated locations compose fine.
            (
                &["etc/pki/entitlement/a.pem"],
                &["etc/rhsm/b.conf"],
                false,
                &[],
            ),
            // Sharing a textual prefix is not sharing a path.
            (&["etc/pki/tls/a.pem"], &["etc/pkix/tls/b.pem"], false, &[]),
        ];

        for (first, second, collides, expected_paths) in cases {
            let fragments = vec![
                mount_fragment_at("rhel-entitlement", first),
                mount_fragment_at("internal-mirror", second),
            ];
            let result = check_mount_overlaps(&fragments);
            assert_eq!(
                result.is_err(),
                *collides,
                "first={first:?} second={second:?}"
            );
            if *collides {
                let err = result.unwrap_err().to_string();
                assert!(
                    err.contains("rhel-entitlement"),
                    "names both fragments: {err}"
                );
                assert!(
                    err.contains("internal-mirror"),
                    "names both fragments: {err}"
                );
                for path in *expected_paths {
                    assert!(
                        err.contains(path),
                        "names the colliding target {path}: {err}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_mount_over_a_generator_written_path_is_an_error() {
        let fragments = vec![mount_fragment_at("broad", &["etc/pki/whatever.pem"])];
        let err = check_mount_overlaps(&fragments)
            .expect_err("mount/etc/pki hides /etc/pki/rpm-gpg for the whole package step")
            .to_string();

        assert!(err.contains("broad"), "names the fragment: {err}");
        assert!(err.contains("/etc/pki"), "names the mount target: {err}");
        assert!(
            err.contains("/etc/pki/rpm-gpg"),
            "names the written path: {err}"
        );
        assert!(
            err.contains("repo files"),
            "names the generator phase: {err}"
        );
    }

    #[test]
    fn a_mount_below_a_generator_written_path_is_allowed() {
        // The generator writes files directly into those directories, so a
        // mount below one of them hides nothing the generator wrote.
        let fragments = vec![mount_fragment_at("narrow", &["etc/pki/rpm-gpg/sub/x.pem"])];
        assert!(check_mount_overlaps(&fragments).is_ok());
    }

    #[test]
    fn a_repo_directory_mount_is_an_error() {
        let fragments = vec![mount_fragment_at(
            "repo-mount",
            &["etc/yum.repos.d/internal.repo"],
        )];
        let err = check_mount_overlaps(&fragments)
            .expect_err("that target equals a path the generator writes")
            .to_string();
        assert!(err.contains("/etc/yum.repos.d"), "got: {err}");
    }

    #[test]
    fn mounts_with_no_package_step_produce_a_notice() {
        let fragments = vec![mount_fragment_at(
            "rhel-entitlement",
            &["etc/rhsm/rhsm.conf"],
        )];
        let empty = manifest_of(vec![ManifestFragment {
            image: "quay.io/acme/rhel-entitlement@sha256:abc".into(),
            packages: vec![],
            mirror: None,
        }]);
        let notice = unattached_mount_notice(&empty, &fragments)
            .expect("build mounts attach to the batched dnf RUN, and there is none");
        assert!(notice.contains("rhel-entitlement"), "got: {notice}");

        let selected = manifest_of(vec![ManifestFragment {
            image: "quay.io/acme/rhel-entitlement@sha256:abc".into(),
            packages: vec!["some-package".into()],
            mirror: None,
        }]);
        assert!(unattached_mount_notice(&selected, &fragments).is_none());

        // No mount-carrying fragment at all: nothing to notice about,
        // regardless of what the manifest installs.
        let no_mounts = vec![test_fragment("plain", vec![], vec![])];
        assert!(
            unattached_mount_notice(&empty, &no_mounts).is_none(),
            "no mount-carrying fragment means nothing to report"
        );

        // A package step driven by the fragment's own packages.required,
        // not the manifest's per-entry selection, still emits the batched
        // dnf RUN the mounts attach to, so the notice must stay silent.
        let mut required_frag = mount_fragment_at("rhel-entitlement", &["etc/rhsm/rhsm.conf"]);
        required_frag.fragment.packages.required = vec!["some-package".into()];
        assert!(
            unattached_mount_notice(&empty, &[required_frag]).is_none(),
            "packages.required still drives a package step"
        );
    }
}
