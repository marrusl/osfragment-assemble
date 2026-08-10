//! Build mounts: the `mount/` directory a fragment may carry, and the
//! derivation from its file paths to the bind mounts the generator attaches
//! to the package step and to every hook step.
//!
//! A bind mount shadows its target directory rather than merging into it, so
//! the derived unit is a directory and never a file: every directory under
//! `mount/` that directly contains a regular file, minus any that is nested
//! inside another such directory.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use crate::fragment::FragmentName;

/// OCI annotation key carrying a fragment's mount targets as a JSON array of
/// absolute paths. Hand-authored by the fragment author on their own
/// `podman build`, like every other annotation this tool reads: there is no
/// publish step to write it.
pub const MOUNTS_ANNOTATION_KEY: &str = "com.github.marrusl.osfragment.mounts";

/// The sentence every surface that renders mount targets closes with, held
/// in one place so `inspect` and `list` cannot drift apart.
pub const MOUNT_SECTION_NOTE: &str =
    "mounted during package and hook steps, never committed by the builder";

/// A path the generator's own package phase writes to before the batched dnf
/// RUN. A mount target that equals or contains one of these hides it for
/// exactly the RUN that needs it.
pub struct GeneratorWrittenPath {
    /// Absolute path in the built image.
    pub path: &'static str,
    /// The generator phase that owns the path, named in the collision error.
    pub phase: &'static str,
}

/// Kept in sync by hand with the repo files section of
/// `generator::generate_containerfile`, which copies into exactly these two
/// directories ahead of the package step.
pub const GENERATOR_WRITTEN_PATHS: &[GeneratorWrittenPath] = &[
    GeneratorWrittenPath {
        path: "/etc/yum.repos.d",
        phase: "repo files",
    },
    GeneratorWrittenPath {
        path: "/etc/pki/rpm-gpg",
        phase: "repo files",
    },
];

/// One derived bind mount: a directory under `mount/`, stored relative with
/// no leading separator, that becomes exactly one `--mount` flag.
///
/// The inner `PathBuf` is private to this module, so the only ways to obtain
/// a `MountPoint` are [`derive_mount_points`], from a fragment's own files,
/// and [`MountPoint::from_target`], which revalidates an annotation's claim.
/// Holding one is proof that it names a relative path of ordinary
/// components, which is what lets the render methods join it onto a prefix
/// without rechecking at each call.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MountPoint(PathBuf);

impl MountPoint {
    /// Parse an absolute target path, the form an OCI annotation carries.
    ///
    /// Rejects rather than sanitizes: an annotation is external input, and
    /// every render method below assumes exactly what this checks.
    pub fn from_target(target: &str) -> Result<Self> {
        let rest = target.strip_prefix('/').unwrap_or("");
        // Checked on the raw segments rather than `Path::components()`: that
        // iterator normalizes an internal `.` away entirely instead of
        // surfacing it as a `CurDir` component, so it cannot distinguish
        // `/etc/pki` from `/etc/./pki`.
        let ordinary = !rest.is_empty()
            && rest
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
        if !ordinary {
            bail!(
                "mount target '{}' is not usable: a target must be an absolute path \
                 of ordinary components, for example /etc/pki/entitlement",
                target.escape_debug()
            );
        }
        Ok(Self(PathBuf::from(rest)))
    }

    /// Absolute path in the built image: `/etc/pki/entitlement`.
    pub fn target(&self) -> String {
        format!("/{}", self.0.display())
    }

    /// Source path inside the fragment image, for an inline `from=` mount.
    pub fn layer_source(&self) -> String {
        format!("/fragment/mount/{}", self.0.display())
    }

    /// Source path inside a self-contained build context, for a mount that
    /// carries no `from=` because the material was materialized on disk.
    pub fn context_source(&self, fragment: &FragmentName) -> String {
        format!("fragments/{}/mount/{}", fragment, self.0.display())
    }

    /// Render this mount point as a `--mount=` flag for a build RUN: the
    /// package step and every hook step both attach it. The option skeleton
    /// and key order (`type=bind`, `from=` optional, `source=`, `target=`,
    /// `ro`, `z`) are defined in exactly this one place, so the generator's
    /// two emission forms cannot drift apart on it.
    ///
    /// `from` is the inline registry reference for the default output
    /// form, or `None` for the self-contained form, which carries no
    /// `from=` and reads from the materialized build context instead.
    /// `source` is the already-derived source path: the caller passes
    /// [`Self::layer_source`] or [`Self::context_source`] depending on
    /// which form it is emitting. This method owns only the flag's
    /// byte-format, not the decision of which form or source to use.
    pub fn mount_flag(&self, from: Option<&str>, source: &str) -> String {
        match from {
            Some(image_ref) => format!(
                "--mount=type=bind,from={},source={},target={},ro,z",
                image_ref,
                source,
                self.target()
            ),
            None => format!(
                "--mount=type=bind,source={},target={},ro,z",
                source,
                self.target()
            ),
        }
    }

    /// Whether two mount targets collide: either equals or is an ancestor of
    /// the other. Comparison is component-wise, so `/etc/pkix` does not
    /// collide with `/etc/pki`.
    pub fn overlaps(&self, other: &MountPoint) -> bool {
        self.0.starts_with(&other.0) || other.0.starts_with(&self.0)
    }

    /// Whether this mount hides `absolute_path` for the duration of the RUN:
    /// true when the target equals or contains it. The reverse nesting is
    /// not a collision, because the generator writes files directly at the
    /// paths it owns and a mount below one of them hides nothing it wrote.
    pub fn shadows(&self, absolute_path: &str) -> bool {
        Path::new(absolute_path.trim_start_matches('/')).starts_with(&self.0)
    }
}

/// Derive one mount point per directory under `mount/` that directly
/// contains a regular file, then drop any that is nested inside another.
///
/// `mount_files` are file paths relative to `mount/`, for example
/// `etc/pki/entitlement/cert.pem`. The result is sorted, so emission order
/// is stable across runs.
pub fn derive_mount_points(
    fragment_name: &str,
    mount_files: &[PathBuf],
) -> Result<Vec<MountPoint>> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for file in mount_files {
        let parent = file.parent().unwrap_or_else(|| Path::new(""));
        if parent.as_os_str().is_empty() {
            bail!(
                "fragment '{}': mount/{} is a regular file directly under mount/, which \
                 would derive a bind mount onto /. A mount point is derived from the \
                 directory that directly contains a file, so a file at the top of mount/ \
                 would mount the filesystem root and every other mount point would be \
                 pruned as nested inside it. Move it to the path it should appear at \
                 during the build, for example \
                 mount/etc/pki/entitlement/{}.",
                fragment_name,
                file.display(),
                file.display()
            );
        }
        let parent = parent.to_path_buf();
        if !dirs.contains(&parent) {
            dirs.push(parent);
        }
    }

    // Ancestor pruning. A bind mount shadows its target directory, so an
    // inner mount would be hidden by the outer one for the whole RUN while
    // still costing a flag and a line of the MachineOSConfig budget.
    let mut kept: Vec<MountPoint> = Vec::new();
    for dir in &dirs {
        let nested = dirs
            .iter()
            .any(|other| other != dir && dir.starts_with(other));
        if !nested {
            kept.push(MountPoint(dir.clone()));
        }
    }
    kept.sort();
    Ok(kept)
}

/// Notice for a fragment carrying a `mount/` directory that holds no regular
/// files and therefore derives no mounts at all. Almost always an authoring
/// mistake, and silence would hide it.
///
/// A pure function returning the text rather than printing it: the callers
/// are a library load path and `inspect`, and only they know when a run
/// should say anything.
pub fn empty_mount_notice(
    fragment_name: &str,
    has_mount_dir: bool,
    derived: &[MountPoint],
) -> Option<String> {
    (has_mount_dir && derived.is_empty()).then(|| {
        format!(
            "notice: fragment '{}' carries a mount/ directory holding no files, so it \
             derives no build mounts and nothing is mounted into the package or hook \
             steps. Put the material at the path it should appear at during the build, \
             for example mount/etc/pki/entitlement/cert.pem.",
            fragment_name
        )
    })
}

/// Warning for a fragment whose mounts annotation disagrees with the mount
/// points derived from its layers.
///
/// The existing annotations cache the in-layer `fragment.toml` and reconcile
/// against it. A mounts annotation has no in-layer file to reconcile
/// against, so its counterpart is the derived targets, and the layer stays
/// authoritative exactly as it is for every other annotation.
pub fn mount_annotation_drift(
    fragment_name: &str,
    annotated: &[MountPoint],
    derived: &[MountPoint],
) -> Option<String> {
    if annotated == derived {
        return None;
    }
    let render = |points: &[MountPoint]| {
        if points.is_empty() {
            "(none)".to_string()
        } else {
            points
                .iter()
                .map(MountPoint::target)
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    Some(format!(
        "warning: fragment '{}' annotates mount targets that do not match its layers. \
         Annotated: {}. Derived from the layer: {}. The layer is authoritative and \
         generation uses the derived targets. Rebuild the fragment with a corrected \
         {} annotation so metadata-only reads agree with it.",
        fragment_name,
        render(annotated),
        render(derived),
        MOUNTS_ANNOTATION_KEY
    ))
}

/// Whether a materialization run writes a fragment's `mount/` subtree into
/// the build context.
///
/// An enum rather than a bool because it crosses three signatures, and
/// `materialize_fragment(image_ref, dest, true)` at a call site says nothing
/// about what is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMaterialization {
    /// Default: `mount/` entries are skipped entirely, and no build-mount
    /// material lands in the context or its archive.
    Skip,
    /// `--materialize-mounts`: `mount/` lands in the context, owner-only.
    Write,
}

impl MountMaterialization {
    /// From the `--materialize-mounts` flag, so `main.rs` stays dispatch.
    pub fn from_flag(materialize_mounts: bool) -> Self {
        if materialize_mounts {
            Self::Write
        } else {
            Self::Skip
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn targets(points: &[MountPoint]) -> Vec<String> {
        points.iter().map(MountPoint::target).collect()
    }

    #[test]
    fn derivation_collects_directories_and_prunes_nested_ones() {
        // (mount/ file paths, expected targets)
        let cases: &[(&[&str], &[&str])] = &[
            (
                &[
                    "etc/pki/entitlement/cert.pem",
                    "etc/pki/entitlement/key.pem",
                ],
                &["/etc/pki/entitlement"],
            ),
            // Two files in one directory tree collapse to the outer directory.
            (
                &["etc/rhsm/rhsm.conf", "etc/rhsm/ca/cert.pem"],
                &["/etc/rhsm"],
            ),
            // Unrelated locations stay separate.
            (
                &["etc/pki/entitlement/cert.pem", "etc/rhsm/rhsm.conf"],
                &["/etc/pki/entitlement", "/etc/rhsm"],
            ),
            // Sibling directories under a common parent that holds no file of
            // its own are both kept: neither is nested inside the other.
            (&["etc/a/one.pem", "etc/b/two.pem"], &["/etc/a", "/etc/b"]),
            // A name that merely shares a prefix is not nested.
            (
                &["etc/pki/one.pem", "etc/pkix/two.pem"],
                &["/etc/pki", "/etc/pkix"],
            ),
            // No files at all derives nothing.
            (&[], &[]),
        ];

        for (input, expected) in cases {
            let derived = derive_mount_points("test-fragment", &files(input))
                .unwrap_or_else(|e| panic!("{input:?} must derive, got: {e}"));
            assert_eq!(targets(&derived), *expected, "input: {input:?}");
        }
    }

    /// The sort at the end of `derive_mount_points` is what makes emission
    /// order stable across runs, and every case in the table above happens to
    /// feed already-sorted input, so none of them can see it go missing. A
    /// tar walk hands files back in whatever order the archive stored them,
    /// which is the input this locks: descending in, ascending out, and two
    /// permutations of one set agreeing with each other.
    #[test]
    fn derivation_sorts_its_output_whatever_order_the_files_arrive_in() {
        let descending = files(&[
            "etc/rhsm/rhsm.conf",
            "etc/pkix/two.pem",
            "etc/pki/one.pem",
            "etc/a/one.pem",
        ]);
        let ascending = files(&[
            "etc/a/one.pem",
            "etc/pki/one.pem",
            "etc/pkix/two.pem",
            "etc/rhsm/rhsm.conf",
        ]);
        let expected = ["/etc/a", "/etc/pki", "/etc/pkix", "/etc/rhsm"];

        let from_descending = derive_mount_points("f", &descending).unwrap();
        assert_eq!(
            targets(&from_descending),
            expected,
            "reversed input must still come back sorted"
        );

        let from_ascending = derive_mount_points("f", &ascending).unwrap();
        assert_eq!(
            from_descending, from_ascending,
            "two orderings of one set of mount files must derive the same list"
        );
    }

    #[test]
    fn a_file_directly_under_mount_is_a_derivation_error() {
        let err = derive_mount_points("rhel-entitlement", &files(&["cert.pem"]))
            .expect_err("a file at the top of mount/ would derive a mount onto /");
        let msg = err.to_string();
        assert!(
            msg.contains("rhel-entitlement"),
            "must name the fragment: {msg}"
        );
        assert!(msg.contains("cert.pem"), "must name the file: {msg}");
        assert!(msg.contains("onto /"), "must state the rule: {msg}");
        assert!(msg.contains("Move it"), "must give the fix: {msg}");
    }

    #[test]
    fn render_forms_cover_every_emission_surface() {
        let point = derive_mount_points("f", &files(&["etc/pki/entitlement/cert.pem"])).unwrap();
        let point = &point[0];
        let name = FragmentName::new("rhel-entitlement").unwrap();

        assert_eq!(point.target(), "/etc/pki/entitlement");
        assert_eq!(point.layer_source(), "/fragment/mount/etc/pki/entitlement");
        assert_eq!(
            point.context_source(&name),
            "fragments/rhel-entitlement/mount/etc/pki/entitlement"
        );
    }

    #[test]
    fn mount_flag_renders_the_option_skeleton_exactly() {
        let point = derive_mount_points("f", &files(&["etc/pki/entitlement/cert.pem"])).unwrap();
        let point = &point[0];
        let name = FragmentName::new("rhel-entitlement").unwrap();

        assert_eq!(
            point.mount_flag(
                Some("quay.io/acme/rhel-entitlement@sha256:d00d"),
                &point.layer_source()
            ),
            "--mount=type=bind,from=quay.io/acme/rhel-entitlement@sha256:d00d,\
             source=/fragment/mount/etc/pki/entitlement,target=/etc/pki/entitlement,ro,z"
        );
        assert_eq!(
            point.mount_flag(None, &point.context_source(&name)),
            "--mount=type=bind,source=fragments/rhel-entitlement/mount/etc/pki/entitlement,\
             target=/etc/pki/entitlement,ro,z"
        );
    }

    #[test]
    fn from_target_accepts_absolute_paths_and_rejects_everything_else() {
        assert_eq!(
            MountPoint::from_target("/etc/pki/entitlement")
                .unwrap()
                .target(),
            "/etc/pki/entitlement"
        );
        for bad in ["etc/pki", "", "/", "/etc/../etc", "/etc/./pki"] {
            assert!(
                MountPoint::from_target(bad).is_err(),
                "'{bad}' must be rejected"
            );
        }
    }

    #[test]
    fn overlap_is_prefix_based_in_both_directions() {
        let outer = MountPoint::from_target("/etc/pki").unwrap();
        let inner = MountPoint::from_target("/etc/pki/entitlement").unwrap();
        let other = MountPoint::from_target("/etc/rhsm").unwrap();
        let lookalike = MountPoint::from_target("/etc/pkix").unwrap();

        assert!(outer.overlaps(&inner), "ancestor collides with descendant");
        assert!(inner.overlaps(&outer), "and the comparison is symmetric");
        assert!(outer.overlaps(&outer), "a target equals itself");
        assert!(!outer.overlaps(&other));
        assert!(
            !outer.overlaps(&lookalike),
            "prefix comparison is component-wise, not textual"
        );
    }

    #[test]
    fn shadows_is_true_only_when_the_mount_contains_the_written_path() {
        let broad = MountPoint::from_target("/etc/pki").unwrap();
        let exact = MountPoint::from_target("/etc/pki/rpm-gpg").unwrap();
        let below = MountPoint::from_target("/etc/pki/rpm-gpg/extra").unwrap();
        let elsewhere = MountPoint::from_target("/etc/rhsm").unwrap();

        assert!(broad.shadows("/etc/pki/rpm-gpg"));
        assert!(exact.shadows("/etc/pki/rpm-gpg"));
        assert!(
            !below.shadows("/etc/pki/rpm-gpg"),
            "the generator writes files directly at that path, so a mount \
             below it hides nothing the generator wrote"
        );
        assert!(!elsewhere.shadows("/etc/pki/rpm-gpg"));
    }

    #[test]
    fn empty_mount_notice_fires_only_for_a_present_but_fileless_directory() {
        let some = derive_mount_points("f", &files(&["etc/rhsm/rhsm.conf"])).unwrap();

        assert!(
            empty_mount_notice("f", false, &[]).is_none(),
            "no mount/ at all"
        );
        assert!(
            empty_mount_notice("f", true, &some).is_none(),
            "mount/ with files"
        );

        let notice = empty_mount_notice("rhel-entitlement", true, &[])
            .expect("a mount/ holding no files is almost always an authoring mistake");
        assert!(
            notice.contains("rhel-entitlement"),
            "must name the fragment: {notice}"
        );
    }

    #[test]
    fn drift_warning_fires_only_when_annotation_and_layer_disagree() {
        let derived = derive_mount_points("f", &files(&["etc/rhsm/rhsm.conf"])).unwrap();
        let agreeing = vec![MountPoint::from_target("/etc/rhsm").unwrap()];
        let disagreeing = vec![MountPoint::from_target("/etc/pki/entitlement").unwrap()];

        assert!(mount_annotation_drift("f", &agreeing, &derived).is_none());

        let warning = mount_annotation_drift("rhel-entitlement", &disagreeing, &derived)
            .expect("disagreement is drift");
        assert!(warning.contains("rhel-entitlement"), "{warning}");
        assert!(
            warning.contains("/etc/pki/entitlement"),
            "names the annotated: {warning}"
        );
        assert!(
            warning.contains("/etc/rhsm"),
            "names the derived: {warning}"
        );
        assert!(
            warning.contains("authoritative"),
            "states which wins: {warning}"
        );
    }

    #[test]
    fn materialization_policy_tracks_the_flag() {
        assert_eq!(
            MountMaterialization::from_flag(true),
            MountMaterialization::Write
        );
        assert_eq!(
            MountMaterialization::from_flag(false),
            MountMaterialization::Skip
        );
    }
}
