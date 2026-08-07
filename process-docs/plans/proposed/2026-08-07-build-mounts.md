# Build Mounts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a fragment carry a `mount/` directory whose contents are bind-mounted onto the generated Containerfile's batched package step, digest-pinned and never committed by the builder, so package acquisition can authenticate from any build host.

**Architecture:** A new module, `src/mount.rs`, owns the whole vocabulary: the `MountPoint` newtype, the derivation from a fragment's `mount/` file paths to bind mount points (directories that directly contain files, minus any nested inside another), the render forms each surface needs, the annotation key, and the pure notice/warning text functions. `src/loader.rs` detects `mount/` in fragment layers alongside the existing tree/hooks walk, derives the mount points once at load, and hangs them on `LoadedFragment`; it also gains an optional materialization of `mount/` to disk for self-contained output. `src/validate.rs` gains the digest-pin and overlap checks, `src/generator.rs` emits one `--mount` flag per derived point on the existing batched dnf RUN and excludes pure-mount fragments from the named-stage loop, and `src/self_contained.rs` gates materialization behind `--materialize-mounts`, writes the mount subtrees owner-only, and emits a `.gitignore`. `src/inspect.rs` and `src/list.rs` render the derived targets.

**Tech Stack:** Rust 2021, `anyhow`, `clap` derive, `serde_json`, `tar` + `flate2`, `tempfile`, `cargo test`

## Global Constraints

- Authoritative source: `process-docs/specs/proposed/2026-08-07-build-mounts.md` at commit `135e240`. Implement exactly what it says; if a step here disagrees with the spec, the spec wins and the disagreement is a finding to report, not something to improvise around.
- `cargo clippy --all-targets -- -D clippy::all` must report zero warnings, and `cargo fmt --check` must pass. Both gate every commit in this plan.
- Conventional commits, imperative mood: `type(scope): description`. Body explains why, not what. Attribution trailer on every commit: `Assisted-by: Claude Code (Opus 5)`. Never push.
- This is a public repository. No team member names anywhere in commits or file content. No em dashes anywhere in repository content, including code comments, error strings, and docs. Avoid the word "shape."
- Annotation key literal, exactly: `com.github.marrusl.osfragment.mounts`. Its value is a JSON array of absolute target paths, for example `["/etc/pki/entitlement"]`.
- Emitted mount options, exactly and in this order: `type=bind`, `from=` (default mode only), `source=`, `target=`, `ro`, `z`. Hook mounts keep their existing `z` without `ro`; that asymmetry is deliberate and no task here changes hook emission.
- Build-mount references are always emitted inline, never as a named stage, including under `--pin-digests`.
- The `--ocp` path wraps the same generated Containerfile, and `src/ocp.rs` rejects content over 4096 characters. Every emitted `--mount` flag is a long line inside that budget, which is part of why pure-mount fragments are excluded from the named-stage loop.
- Fragments are emitted in manifest order everywhere, and derived mount points are sorted, so generation is byte-stable across runs.
- Symlink and hardlink rejection under `mount/` needs no new code: `validate_tar_entry` in `src/loader.rs` already rejects both for every entry in every fragment layer, before any prefix matching happens. Task 3 adds the test that pins this for `mount/` specifically.
- Target paths carry no other policy: no expected-path list, no warning for unusual paths.
- Tests are inline `#[cfg(test)]` modules in each `src/*.rs` file. `tests/cli.rs` is offline only; `inspect <local-dir>` against `examples/fragments/*` is the one subcommand reachable there, plus argument-parsing failures.
- Line numbers below are as of commit `135e240` and shift as tasks land. Treat them as a starting point, and locate the code by the quoted text.

---

### Task 1: The `mount/` vocabulary module

**Files:**
- Create: `src/mount.rs`
- Modify: `src/lib.rs` (module list, 9 lines)

**Interfaces:**
- Consumes: `crate::fragment::FragmentName` (existing newtype).
- Produces, all consumed by later tasks:
  - `pub struct MountPoint(PathBuf)` with `pub fn from_target(target: &str) -> Result<Self>`, `pub fn target(&self) -> String`, `pub fn layer_source(&self) -> String`, `pub fn context_source(&self, fragment: &FragmentName) -> String`, `pub fn overlaps(&self, other: &MountPoint) -> bool`, `pub fn shadows(&self, absolute_path: &str) -> bool`
  - `pub fn derive_mount_points(fragment_name: &str, mount_files: &[PathBuf]) -> Result<Vec<MountPoint>>`
  - `pub fn empty_mount_notice(fragment_name: &str, has_mount_dir: bool, derived: &[MountPoint]) -> Option<String>`
  - `pub fn mount_annotation_drift(fragment_name: &str, annotated: &[MountPoint], derived: &[MountPoint]) -> Option<String>`
  - `pub enum MountMaterialization { Skip, Write }` with `pub fn from_flag(materialize_mounts: bool) -> Self`
  - `pub const MOUNTS_ANNOTATION_KEY: &str`, `pub const MOUNT_SECTION_NOTE: &str`
  - `pub struct GeneratorWrittenPath { pub path: &'static str, pub phase: &'static str }` and `pub const GENERATOR_WRITTEN_PATHS: &[GeneratorWrittenPath]`

- [ ] **Step 1: Register the module**

In `src/lib.rs`, add `pub mod mount;` in alphabetical order so the file reads:

```rust
pub mod fragment;
pub mod generator;
pub mod inspect;
pub mod list;
pub mod loader;
pub mod manifest;
pub mod mount;
pub mod ocp;
pub mod self_contained;
pub mod validate;
```

- [ ] **Step 2: Write the failing tests**

Create `src/mount.rs` containing only the module doc comment and this test module. It will not compile yet, which is the failure this step wants.

```rust
//! Build mounts: the `mount/` directory a fragment may carry, and the
//! derivation from its file paths to the bind mounts the generator attaches
//! to the batched package step.
//!
//! A bind mount shadows its target directory rather than merging into it, so
//! the derived unit is a directory and never a file: every directory under
//! `mount/` that directly contains a regular file, minus any that is nested
//! inside another such directory.

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
                &["etc/pki/entitlement/cert.pem", "etc/pki/entitlement/key.pem"],
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
            (
                &["etc/a/one.pem", "etc/b/two.pem"],
                &["/etc/a", "/etc/b"],
            ),
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

    #[test]
    fn a_file_directly_under_mount_is_a_derivation_error() {
        let err = derive_mount_points("rhel-entitlement", &files(&["cert.pem"]))
            .expect_err("a file at the top of mount/ would derive a mount onto /");
        let msg = err.to_string();
        assert!(msg.contains("rhel-entitlement"), "must name the fragment: {msg}");
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
    fn from_target_accepts_absolute_paths_and_rejects_everything_else() {
        assert_eq!(
            MountPoint::from_target("/etc/pki/entitlement").unwrap().target(),
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

        assert!(empty_mount_notice("f", false, &[]).is_none(), "no mount/ at all");
        assert!(empty_mount_notice("f", true, &some).is_none(), "mount/ with files");

        let notice = empty_mount_notice("rhel-entitlement", true, &[])
            .expect("a mount/ holding no files is almost always an authoring mistake");
        assert!(notice.contains("rhel-entitlement"), "must name the fragment: {notice}");
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
        assert!(warning.contains("/etc/pki/entitlement"), "names the annotated: {warning}");
        assert!(warning.contains("/etc/rhsm"), "names the derived: {warning}");
        assert!(warning.contains("authoritative"), "states which wins: {warning}");
    }

    #[test]
    fn materialization_policy_tracks_the_flag() {
        assert_eq!(MountMaterialization::from_flag(true), MountMaterialization::Write);
        assert_eq!(MountMaterialization::from_flag(false), MountMaterialization::Skip);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test --lib mount::
```

Expected: compilation errors, `cannot find function derive_mount_points in this scope` and similar for every item the test module names.

- [ ] **Step 4: Write the implementation**

Insert this above the `#[cfg(test)]` module in `src/mount.rs`:

```rust
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
    "mounted during the package step, never committed by the builder";

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
        let path = Path::new(rest);
        let ordinary = path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        if rest.is_empty() || !ordinary {
            bail!(
                "mount target '{}' is not usable: a target must be an absolute path \
                 of ordinary components, for example /etc/pki/entitlement",
                target.escape_debug()
            );
        }
        Ok(Self(path.to_path_buf()))
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
                 during the package step, for example \
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
             derives no build mounts and nothing is mounted into the package step. Put \
             the material at the path it should appear at during that step, for example \
             mount/etc/pki/entitlement/cert.pem.",
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
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test --lib mount:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

Expected: 8 tests pass, formatting clean, zero clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add src/mount.rs src/lib.rs
git commit -m "feat(mount): add the build-mount derivation vocabulary

A bind mount shadows its target directory rather than merging into it, so
the derived unit has to be a directory and never a file. Deriving from the
directories that directly contain files, then pruning the nested ones, is
the rule that makes one mount flag per location, and it needs a type that
proves its own invariants before four call sites start joining paths onto it.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 2: Make digest pinning idempotent on an already-pinned reference

**Files:**
- Modify: `src/generator.rs` (add after `split_image_ref`, around line 29)
- Modify: `src/loader.rs` (`load_registry_fragment` around line 531, `load_registry_fragment_metadata_only` around line 562)

**Interfaces:**
- Consumes: `pub fn split_image_ref(image_ref: &str) -> (&str, Option<&str>)` (existing, `src/generator.rs:14`).
- Produces: `pub fn pin_to_digest(image_ref: &str, digest: &str) -> String`, consumed by `src/loader.rs` in this task and relied on by every later task that reads `LoadedFragment::source`.

Why this comes before the feature: `split_image_ref` returns a digest-bearing reference whole, because a digest is not a tag. Both loader call sites then format `"{name}@{digest}"` around that result, so a manifest entry that already carries `@sha256:...` produces `quay.io/acme/x@sha256:abc@sha256:abc`. Build-mount fragments must be pinned in the manifest, which turns that from an edge case into the normal case for this feature.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/generator.rs`:

```rust
    #[test]
    fn pinning_replaces_any_existing_tag_or_digest() {
        // (input ref, expected pinned form)
        let cases = [
            (
                "quay.io/acme/frag:1.0",
                "quay.io/acme/frag@sha256:beef",
            ),
            ("quay.io/acme/frag", "quay.io/acme/frag@sha256:beef"),
            (
                "localhost:5000/acme/frag:1.0",
                "localhost:5000/acme/frag@sha256:beef",
            ),
            // The case build mounts make routine: the manifest already pins,
            // and pinning again must not append a second digest.
            (
                "quay.io/acme/frag@sha256:cafe",
                "quay.io/acme/frag@sha256:beef",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(pin_to_digest(input, "sha256:beef"), expected, "input: {input}");
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib generator::tests::pinning_replaces_any_existing_tag_or_digest
```

Expected: `cannot find function pin_to_digest in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/generator.rs`, immediately after the closing brace of `split_image_ref`:

```rust
/// Rewrite `image_ref` to name `digest`, dropping whatever tag or digest it
/// already carries.
///
/// `split_image_ref` deliberately returns a digest-bearing reference whole,
/// because a digest is not a tag, so formatting `{name}@{digest}` around its
/// result appends a second digest to a reference that already had one. Build
/// mounts require the manifest to pin, which makes an already-pinned
/// reference the normal input here rather than an unusual one.
pub fn pin_to_digest(image_ref: &str, digest: &str) -> String {
    let (name, _tag) = split_image_ref(image_ref);
    let repository = name.split('@').next().unwrap_or(name);
    format!("{}@{}", repository, digest)
}
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test --lib generator::tests::pinning_replaces_any_existing_tag_or_digest
```

- [ ] **Step 5: Use it at both loader call sites**

In `src/loader.rs`, change the import on line 9 from:

```rust
use crate::generator::split_image_ref;
```

to:

```rust
use crate::generator::{pin_to_digest, split_image_ref};
```

In `load_registry_fragment`, replace:

```rust
    let digest = resolve_digest(image_ref)?;
    let (name, _tag) = split_image_ref(image_ref);
    let image_with_digest = format!("{}@{}", name, digest);
```

with:

```rust
    let digest = resolve_digest(image_ref)?;
    let image_with_digest = pin_to_digest(image_ref, &digest);
```

In `load_registry_fragment_metadata_only`, replace the identical three lines with the identical two lines.

- [ ] **Step 6: Verify the whole suite still passes**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

Expected: all tests pass. If `split_image_ref` is now unused in `src/loader.rs`, clippy reports an unused import; in that case drop it from the import list, keeping `pin_to_digest`.

- [ ] **Step 7: Commit**

```bash
git add src/generator.rs src/loader.rs
git commit -m "fix(loader): pin a digest without appending a second one

split_image_ref returns a digest-bearing reference whole, so formatting
{name}@{digest} around it doubled the digest for any manifest entry that
was already pinned. Build mounts require the manifest to pin, so this stops
being reachable only by manifests that pinned voluntarily.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 3: Detect `mount/` in fragment layers

**Files:**
- Modify: `src/loader.rs` (`LoadedFragment` at line 13, `extract_tree_paths_from_bytes` at line 209, `LayeredMetadata` at line 458, `fragment_from_layers` at line 472, both `LoadedFragment` constructions at lines 542 and 570)
- Modify: `src/generator.rs`, `src/validate.rs`, `src/self_contained.rs` (test fixture literals only)

**Interfaces:**
- Consumes: `derive_mount_points`, `empty_mount_notice`, `MountPoint` (Task 1).
- Produces:
  - `LoadedFragment.mount_points: Vec<MountPoint>`, read by Tasks 6, 7, 8, 9, 10, 12, 13, 14, 16, 17
  - `pub fn LoadedFragment::is_pure_mount(&self) -> bool`, read by Task 9
  - private `struct LayerEntries { file_paths: Vec<PathBuf>, entrypoint_mode: Option<u32>, has_mount_dir: bool }`
  - private `fn extract_layer_entries(compressed: &[u8]) -> Result<LayerEntries>`, replacing `extract_tree_paths_from_bytes`

This task is one compile unit: adding a field to `LoadedFragment` breaks every struct literal at once, so the fixture updates land with it.

- [ ] **Step 1: Write the failing tests**

Add to `mod layer_tests` in `src/loader.rs` (the module that already owns `create_test_tarball`, `RawEntry`, and the `fragment_layers` helper, which takes `Vec<RawEntry>` and prepends a `fragment.toml` entry of its own):

```rust
    /// A `mount/` entry at the mode credential material usually carries.
    /// Mirrors the existing `hook_entry` helper directly above.
    fn mount_entry<'a>(path: &'a str, data: &'a [u8]) -> RawEntry<'a> {
        RawEntry {
            path: path.as_bytes(),
            data,
            mode: 0o600,
            entry_type: tar::EntryType::Regular,
        }
    }

    #[test]
    fn mount_paths_derive_mount_points_at_load() {
        let layers = fragment_layers(vec![
            mount_entry("fragment/mount/etc/pki/entitlement/cert.pem", b"cert"),
            mount_entry("fragment/mount/etc/pki/entitlement/key.pem", b"key"),
            mount_entry("fragment/mount/etc/rhsm/rhsm.conf", b"conf"),
            mount_entry("fragment/mount/etc/rhsm/ca/ca.pem", b"ca"),
        ]);
        let metadata = fragment_from_layers(&layers).expect("a pure mount fragment loads");

        let targets: Vec<String> = metadata
            .mount_points
            .iter()
            .map(crate::mount::MountPoint::target)
            .collect();
        assert_eq!(
            targets,
            vec!["/etc/pki/entitlement", "/etc/rhsm"],
            "nested directories are pruned into their ancestor"
        );
    }

    #[test]
    fn a_file_directly_under_mount_fails_the_load() {
        let layers = fragment_layers(vec![mount_entry("fragment/mount/cert.pem", b"cert")]);
        let err = fragment_from_layers(&layers)
            .expect_err("a file at the top of mount/ derives a mount onto /")
            .to_string();
        assert!(err.contains("onto /"), "got: {err}");
    }

    #[test]
    fn a_symlink_under_mount_is_rejected_by_the_shared_entry_validation() {
        // Documentation of existing enforcement rather than new behavior:
        // validate_tar_entry runs on every entry of every layer, before any
        // mount/ prefix matching happens.
        let tarball = create_test_tarball_with_modes(&[RawEntry {
            path: b"fragment/mount/etc/pki/entitlement/cert.pem",
            data: b"",
            mode: 0o644,
            entry_type: tar::EntryType::Symlink,
        }]);
        let err = extract_layer_entries(&tarball)
            .expect_err("links are rejected anywhere in a fragment layer")
            .to_string();
        assert!(err.contains("symlink or hardlink"), "got: {err}");
    }

    #[test]
    fn an_empty_mount_directory_is_detected_for_the_notice() {
        let tarball = create_test_tarball_with_modes(&[RawEntry {
            path: b"fragment/mount/",
            data: b"",
            mode: 0o755,
            entry_type: tar::EntryType::Directory,
        }]);
        let entries = extract_layer_entries(&tarball).expect("a directory entry is valid");
        assert!(entries.file_paths.is_empty());
        assert!(
            entries.has_mount_dir,
            "a mount/ holding no files is invisible in file_paths, and that is \
             exactly the case the empty-mount notice exists to catch"
        );
    }

    #[test]
    fn a_fragment_carrying_only_metadata_and_mount_is_pure_mount() {
        // (tree paths under tree/, hook paths, has mounts, expected)
        let cases: &[(&[&str], &[&str], bool, bool)] = &[
            (&[], &[], true, true),
            (&[], &[], false, false),
            (&["tree/etc/yum.repos.d/x.repo"], &[], true, false),
            (&[], &["entrypoint"], true, false),
        ];
        for (tree, hooks, has_mounts, expected) in cases {
            let mut loaded = stub_loaded("quay.io/test/frag:1");
            loaded.tree_paths = tree.iter().map(PathBuf::from).collect();
            loaded.hook_paths = hooks.iter().map(PathBuf::from).collect();
            loaded.mount_points = if *has_mounts {
                crate::mount::derive_mount_points("frag", &[PathBuf::from("etc/rhsm/rhsm.conf")])
                    .unwrap()
            } else {
                vec![]
            };
            assert_eq!(
                loaded.is_pure_mount(),
                *expected,
                "tree={tree:?} hooks={hooks:?} mounts={has_mounts}"
            );
        }
    }
```

`stub_loaded` lives in `mod tests`, not `mod layer_tests`. Put the last test in `mod tests` instead, and the first four in `mod layer_tests`.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib loader::
```

Expected: `no field mount_points on type LayeredMetadata`, `cannot find function extract_layer_entries`, `no method named is_pure_mount`.

- [ ] **Step 3: Add the layer walk**

In `src/loader.rs`, add to the imports after line 10:

```rust
use crate::mount::{derive_mount_points, empty_mount_notice, MountPoint};
```

Add the constant next to `HOOKS_ENTRYPOINT_TAR_PATH` (around line 75):

```rust
/// A fragment's build-mount subtree, as it appears inside a fragment layer.
/// Everything below it mirrors a target path, the same convention `tree/`
/// uses.
const MOUNT_TAR_PREFIX: &str = "fragment/mount";
```

Replace `extract_tree_paths_from_bytes` (lines 204 to 227) wholesale with:

```rust
/// What one pass over a layer's tar entries yields.
struct LayerEntries {
    /// Regular-file paths, in the canonical form `validate_tar_entry`
    /// returns.
    file_paths: Vec<PathBuf>,
    /// Mode of `fragment/hooks/entrypoint` when this layer carries one as a
    /// regular file.
    entrypoint_mode: Option<u32>,
    /// Whether this layer carries a `fragment/mount` entry of any type. A
    /// `mount/` holding no files leaves nothing in `file_paths`, so the
    /// directory entry is the only evidence the empty-mount notice has.
    has_mount_dir: bool,
}

/// Walk a layer once, collecting everything downstream needs from it.
///
/// The entrypoint mode comes off the header this loop already holds, so
/// enforcing the entrypoint contract costs no second pass, and the same is
/// true of the mount directory check.
fn extract_layer_entries(compressed: &[u8]) -> Result<LayerEntries> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = LayerEntries {
        file_paths: Vec::new(),
        entrypoint_mode: None,
        has_mount_dir: false,
    };

    for entry_result in archive.entries()? {
        let entry = entry_result?;
        let path = entry.path()?.to_path_buf();
        let path = validate_tar_entry(&path, entry.header().entry_type())?;
        if path.starts_with(MOUNT_TAR_PREFIX) {
            entries.has_mount_dir = true;
        }
        if entry.header().entry_type().is_file() {
            if path == Path::new(HOOKS_ENTRYPOINT_TAR_PATH) {
                entries.entrypoint_mode = Some(entry.header().mode()?);
            }
            entries.file_paths.push(path);
        }
    }
    Ok(entries)
}
```

- [ ] **Step 4: Derive at load and carry the result**

In `src/loader.rs`, add the field to `LoadedFragment` (after `repo_file_contents`, line 24):

```rust
    /// Bind mount points derived from this fragment's `mount/` subtree, in
    /// sorted order. Derived once here so every consumer sees the same
    /// answer and the derivation error surfaces at load.
    pub mount_points: Vec<MountPoint>,
```

Add the method immediately after the struct:

```rust
impl LoadedFragment {
    /// A fragment consisting of metadata and `mount/` alone: it carries
    /// build mounts and nothing a `COPY` or a hook `RUN` could reference.
    /// Build-mount references are always emitted inline, so a named stage
    /// for one of these would be consumed by nothing while still spending
    /// characters against the MachineOSConfig content limit.
    pub fn is_pure_mount(&self) -> bool {
        !self.mount_points.is_empty()
            && self.hook_paths.is_empty()
            && !self
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().starts_with("tree/"))
    }
}
```

Add the field to `LayeredMetadata` (after `repo_file_contents`, line 463):

```rust
    mount_points: Vec<MountPoint>,
```

In `fragment_from_layers`, replace the loop body's first lines. The existing:

```rust
        let (tree_paths, layer_entrypoint_mode) = extract_tree_paths_from_bytes(layer_bytes)?;
        // Later layers shadow earlier ones, so the last entrypoint wins.
        if layer_entrypoint_mode.is_some() {
            entrypoint_mode = layer_entrypoint_mode;
        }
```

becomes:

```rust
        let entries = extract_layer_entries(layer_bytes)?;
        let tree_paths = entries.file_paths;
        // Later layers shadow earlier ones, so the last entrypoint wins.
        if entries.entrypoint_mode.is_some() {
            entrypoint_mode = entries.entrypoint_mode;
        }
        has_mount_dir |= entries.has_mount_dir;

        let layer_mount_files: Vec<PathBuf> = tree_paths
            .iter()
            .filter_map(|p| p.strip_prefix(MOUNT_TAR_PREFIX).ok())
            .map(|p| p.to_path_buf())
            .collect();
        all_mount_files.extend(layer_mount_files);
```

Declare the two new accumulators next to `let mut entrypoint_mode = None;` (line 476):

```rust
    let mut has_mount_dir = false;
    let mut all_mount_files: Vec<PathBuf> = Vec::new();
```

After the existing `validate_hooks_entrypoint` block (line 520), add:

```rust
    let mount_points = derive_mount_points(fragment.name.as_str(), &all_mount_files)?;
    if let Some(notice) = empty_mount_notice(fragment.name.as_str(), has_mount_dir, &mount_points) {
        eprintln!("{}", notice);
    }
```

Add `mount_points,` to the returned `LayeredMetadata`.

- [ ] **Step 5: Fill the field at both construction sites and in every fixture**

In `src/loader.rs::load_registry_fragment`, after `repo_file_contents: metadata.repo_file_contents,`:

```rust
        mount_points: metadata.mount_points,
```

In `src/loader.rs::load_registry_fragment_metadata_only`, after `repo_file_contents: std::collections::HashMap::new(),`:

```rust
            mount_points: vec![],
```

(Task 4 replaces that `vec![]` with the annotated targets.)

Then add `mount_points: vec![],` immediately after the `repo_file_contents:` line in every remaining `LoadedFragment` literal. Find them all with:

```bash
rg -n 'repo_file_contents: std::collections::HashMap::new\(\),' src/
```

Expected sites: `src/loader.rs` (`stub_loaded`), `src/generator.rs` (`make_repos_fragment`, `make_config_fragment`, `make_hook_fragment`, `make_unpinned_repos_fragment`, `make_unpinned_config_fragment`, and the literal inside `self_contained_hooks_only_fragment_has_no_tree_copy`), `src/validate.rs` (`test_fragment`), `src/self_contained.rs` (`test_loaded_fragment`).

- [ ] **Step 6: Fix the remaining call site of the renamed function**

`src/loader.rs` around line 1093 has:

```rust
        let (paths, _entrypoint_mode) = extract_tree_paths_from_bytes(&tarball).unwrap();
```

Replace with:

```rust
        let paths = extract_layer_entries(&tarball).unwrap().file_paths;
```

- [ ] **Step 7: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 8: Commit**

```bash
git add src/loader.rs src/generator.rs src/validate.rs src/self_contained.rs
git commit -m "feat(loader): detect mount/ in fragment layers

Detection is presence-based, exactly like repo files: no new fragment.toml
section, no phase vocabulary, no new fragment kind. Deriving the mount
points once at load keeps every consumer reading the same answer and puts
the derivation error where the fragment is named.

The layer walk now returns a struct rather than a tuple, because an empty
mount/ leaves no file path behind and the directory entry is the only
evidence a notice can be built from.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 4: Read mount targets from the OCI annotation

**Files:**
- Modify: `src/loader.rs` (`try_annotation_fast_path` at line 318, `load_registry_fragment_metadata_only` at line 561)

**Interfaces:**
- Consumes: `MountPoint::from_target`, `MOUNTS_ANNOTATION_KEY` (Task 1); `LoadedFragment.mount_points` (Task 3).
- Produces:
  - private `fn fetch_annotations(image_ref: &str) -> Result<Option<serde_json::Value>>`, consumed by Task 5
  - private `fn mounts_from_annotations(annotations: &serde_json::Value) -> Option<Vec<MountPoint>>`, consumed by Task 5
  - `try_annotation_fast_path` now returns `Result<Option<(Fragment, Vec<MountPoint>)>>`

- [ ] **Step 1: Write the failing test**

Add to `mod layer_tests` in `src/loader.rs`, next to the existing `fragment_from_annotations` tests:

```rust
    #[test]
    fn mounts_annotation_parses_into_sorted_mount_points() {
        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.mounts": "[\"/etc/rhsm\", \"/etc/pki/entitlement\"]"
        });
        let mounts = mounts_from_annotations(&annotations)
            .expect("the key is present, so the answer is a list and not absence");
        let targets: Vec<String> = mounts.iter().map(crate::mount::MountPoint::target).collect();
        assert_eq!(
            targets,
            vec!["/etc/pki/entitlement", "/etc/rhsm"],
            "sorted, so a comparison against derived targets is order-independent"
        );
    }

    #[test]
    fn an_absent_mounts_annotation_is_absence_not_an_empty_list() {
        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.name": "epel"
        });
        assert!(
            mounts_from_annotations(&annotations).is_none(),
            "absence is not drift: the annotation is optional"
        );
    }

    #[test]
    fn a_malformed_mounts_annotation_drops_the_unusable_entries() {
        // The annotations are a cache of what the layer says, and the layer
        // is authoritative, so a bad entry costs its own visibility and
        // nothing else.
        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.mounts": "[\"/etc/rhsm\", \"etc/relative\", \"/\"]"
        });
        let mounts = mounts_from_annotations(&annotations).expect("the key is present");
        let targets: Vec<String> = mounts.iter().map(crate::mount::MountPoint::target).collect();
        assert_eq!(targets, vec!["/etc/rhsm"]);
    }

    #[test]
    fn a_mounts_annotation_that_is_not_json_reads_as_absent() {
        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.mounts": "not json at all"
        });
        assert!(mounts_from_annotations(&annotations).is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib loader::layer_tests::mounts
```

Expected: `cannot find function mounts_from_annotations in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/loader.rs`, extend the `crate::mount` import to:

```rust
use crate::mount::{derive_mount_points, empty_mount_notice, MountPoint, MOUNTS_ANNOTATION_KEY};
```

Split the registry call out of `try_annotation_fast_path`. Replace the whole function (lines 316 to 341) with:

```rust
/// Fetch an image's OCI manifest annotations.
///
/// `Ok(None)` when the registry call fails or the manifest carries no
/// annotations at all. Failure is not an error here: every caller treats
/// annotations as a cache over authoritative layer content and has a path
/// that works without them.
fn fetch_annotations(image_ref: &str) -> Result<Option<serde_json::Value>> {
    let output = std::process::Command::new("skopeo")
        .args([
            "inspect",
            "--override-os",
            "linux",
            "--raw",
            &format!("docker://{}", image_ref),
        ])
        .output()
        .context("failed to run skopeo inspect --raw")?;

    if !output.status.success() {
        return Ok(None);
    }

    let manifest: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(manifest.get("annotations").cloned())
}

/// Try the OCI annotation fast path: parse fragment metadata and mount
/// targets from manifest annotations without pulling any layers.
fn try_annotation_fast_path(image_ref: &str) -> Result<Option<(Fragment, Vec<MountPoint>)>> {
    let annotations = match fetch_annotations(image_ref)? {
        Some(a) => a,
        None => return Ok(None),
    };
    let mounts = mounts_from_annotations(&annotations).unwrap_or_default();
    Ok(fragment_from_annotations(&annotations).map(|fragment| (fragment, mounts)))
}

/// Mount targets from the mounts annotation.
///
/// `None` means the key is absent, which is not drift: the annotation is
/// optional and a fragment that never annotated its mounts is simply one
/// `list` has to pull. An entry that does not parse as an absolute target is
/// dropped rather than failing the read, matching how every other annotation
/// here degrades toward the authoritative layer content.
fn mounts_from_annotations(annotations: &serde_json::Value) -> Option<Vec<MountPoint>> {
    let raw = annotations.get(MOUNTS_ANNOTATION_KEY)?.as_str()?;
    let targets: Vec<String> = serde_json::from_str(raw).ok()?;
    let mut mounts: Vec<MountPoint> = targets
        .iter()
        .filter_map(|t| MountPoint::from_target(t).ok())
        .collect();
    mounts.sort();
    Some(mounts)
}
```

In `load_registry_fragment_metadata_only`, change:

```rust
    if let Some(fragment) = try_annotation_fast_path(image_ref)? {
```

to:

```rust
    if let Some((fragment, mount_points)) = try_annotation_fast_path(image_ref)? {
```

and change the `mount_points: vec![],` line added in Task 3 to:

```rust
            mount_points,
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/loader.rs
git commit -m "feat(loader): read mount targets from the mounts annotation

The no-pull benefit belongs to list, and only when the annotation is
present. Absence stays absence rather than an empty list, because the two
mean different things to the drift check that follows.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 5: Warn when the annotation drifts from the layer

**Files:**
- Modify: `src/loader.rs` (`load_registry_fragment` around line 530)

**Interfaces:**
- Consumes: `mount_annotation_drift` (Task 1), `fetch_annotations` and `mounts_from_annotations` (Task 4), `LayeredMetadata.mount_points` (Task 3).
- Produces: no new signature. Behavior only: `load_registry_fragment` prints a warning to stderr when a present mounts annotation disagrees with the derived targets.

The check runs only where the layer is already being pulled, which is the spec's condition. `load_registry_fragment_metadata_only` never reaches here on its fast path, so an annotated fragment that skips the pull is never cross-checked, exactly as intended.

- [ ] **Step 1: Write the failing test**

Add to `mod layer_tests` in `src/loader.rs`:

```rust
    #[test]
    fn drift_check_compares_annotated_targets_against_derived_ones() {
        // The wiring in load_registry_fragment needs a registry, so this
        // pins the decision the wiring delegates to, over the two values it
        // passes: what the annotation says and what the layers derive.
        let layers = fragment_layers(vec![mount_entry(
            "fragment/mount/etc/rhsm/rhsm.conf",
            b"conf",
        )]);
        let derived = fragment_from_layers(&layers).unwrap().mount_points;

        let annotations = serde_json::json!({
            "com.github.marrusl.osfragment.mounts": "[\"/etc/pki/entitlement\"]"
        });
        let annotated = mounts_from_annotations(&annotations).expect("the key is present");

        let warning = crate::mount::mount_annotation_drift("epel", &annotated, &derived)
            .expect("annotated and derived disagree");
        assert!(warning.contains("/etc/rhsm"), "got: {warning}");
        assert!(warning.contains("/etc/pki/entitlement"), "got: {warning}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib loader::layer_tests::drift_check_compares
```

Expected: `no field mount_points on type LayeredMetadata` is already fixed, so this fails on `mount_annotation_drift` being unimported at the path used, or passes trivially if Task 1 and Task 4 both landed. If it passes at this point, that is expected: the test pins the pure decision, and Step 3 adds the wiring that the next step's manual check covers.

- [ ] **Step 3: Wire the check into the pull path**

In `src/loader.rs::load_registry_fragment`, after:

```rust
    let layer_bytes_list = pull_layer_bytes(&image_with_digest)?;
    let metadata = fragment_from_layers(&layer_bytes_list)?;
```

add:

```rust
    // The layer is already pulled here, which is the spec's condition for
    // cross-checking. Existing annotations reconcile against the in-layer
    // fragment.toml; a mounts annotation has no in-layer file, so its
    // counterpart is the derived targets themselves. Best effort: a registry
    // hiccup on a metadata read must not fail a generation whose
    // authoritative content is already in hand.
    if let Ok(Some(annotations)) = fetch_annotations(&image_with_digest) {
        if let Some(annotated) = mounts_from_annotations(&annotations) {
            if let Some(warning) = crate::mount::mount_annotation_drift(
                metadata.fragment.name.as_str(),
                &annotated,
                &metadata.mount_points,
            ) {
                eprintln!("{}", warning);
            }
        }
    }
```

- [ ] **Step 4: Verify the suite still passes**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

Expected: all tests pass. Clippy may suggest collapsing the nested `if let` chain; if it does, collapse it exactly as clippy directs rather than adding an allow.

- [ ] **Step 5: Commit**

```bash
git add src/loader.rs
git commit -m "feat(loader): warn when the mounts annotation drifts from the layer

A mounts annotation has no in-layer file to reconcile against, so its
counterpart is the derived targets. Layer content stays authoritative,
matching the cache semantics every other annotation already follows, and
the check runs only where the layer is pulled anyway.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 6: Refuse an unpinned build-mount reference

**Files:**
- Modify: `src/validate.rs` (`validate_composition` at line 7)

**Interfaces:**
- Consumes: `LoadedFragment.mount_points` (Task 3), `Manifest`/`ManifestFragment` (existing, `src/manifest.rs`).
- Produces: `pub fn check_mount_digest_pins(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()>`, called from `validate_composition`.

Note that `validate_composition`'s first parameter is currently `_manifest`. This task is what starts using it, so rename it to `manifest`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/validate.rs`:

```rust
    fn mount_fragment(name: &str, image: &str) -> (LoadedFragment, ManifestFragment) {
        let mut loaded = test_fragment(name, vec![], vec![]);
        loaded.source = FragmentSource::Registry {
            image_ref: image.to_string(),
        };
        loaded.mount_points =
            crate::mount::derive_mount_points(name, &[PathBuf::from("etc/pki/entitlement/cert.pem")])
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

        assert!(err.contains("rhel-entitlement"), "names the fragment: {err}");
        assert!(
            err.contains("image: quay.io/acme/rhel-entitlement@sha256:..."),
            "prints the corrected image: literal to write: {err}"
        );
        assert!(err.contains("skopeo inspect"), "shows how to obtain a digest: {err}");
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
```

Add the imports the fixtures need at the top of `mod tests`:

```rust
    use crate::manifest::{Manifest, ManifestFragment};
    use std::path::PathBuf;
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib validate::
```

Expected: `cannot find function check_mount_digest_pins in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/validate.rs`, change the entry point:

```rust
pub fn validate_composition(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    check_duplicate_names(fragments)?;
    check_conflicts(fragments)?;
    check_repo_conflicts(fragments)?;
    check_mount_digest_pins(manifest, fragments)?;
    Ok(())
}
```

Add at the end of the file, before `#[cfg(test)]`:

```rust
/// A build-mount fragment referenced without a digest is a generation error.
///
/// A movable tag on an artifact that injects trust material into the package
/// step is an invisible substitution point: whoever can move the tag can
/// swap a CA bundle or a credential and redirect the entire package fetch.
/// The pin is checked against the manifest's own image reference, so it
/// survives regardless of `--pin-digests` and needs no per-fragment
/// retention machinery.
pub fn check_mount_digest_pins(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
    for f in fragments {
        if f.mount_points.is_empty() {
            continue;
        }
        let declared = &manifest.fragments[f.manifest_index].image;
        if declared.contains("@sha256:") {
            continue;
        }
        let (repository, _tag) = crate::generator::split_image_ref(declared);
        bail!(
            "fragment '{}' carries build mounts but its manifest entry is not pinned to a \
             digest: {}. A movable tag on an artifact that injects trust material into the \
             package step is an invisible substitution point: whoever can move the tag can \
             swap a credential or a CA bundle and redirect the whole package fetch. Pin it \
             by digest in the manifest:\n\
             \x20   image: {}@sha256:...\n\
             Obtain the digest with:\n\
             \x20   skopeo inspect --format '{{{{.Digest}}}}' docker://{}",
            f.fragment.name,
            declared,
            repository,
            declared
        );
    }
    Ok(())
}
```

The `{{{{.Digest}}}}` escaping is deliberate: a `format!` literal collapses each doubled brace, so the printed text is `{{.Digest}}`, which is what a shell needs.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib validate:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/validate.rs
git commit -m "feat(validate): require a digest pin on build-mount fragments

Pinning by digest is the verifiable control over what gets injected into
the package step, and this is the one deliberate asymmetry with ordinary
fragments. The check reads the manifest's own reference, so the pin
survives regardless of --pin-digests.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 7: Refuse colliding mount targets

**Files:**
- Modify: `src/validate.rs` (`validate_composition`, then a new function at the end)

**Interfaces:**
- Consumes: `MountPoint::overlaps`, `MountPoint::shadows`, `GENERATOR_WRITTEN_PATHS` (Task 1); `LoadedFragment.mount_points` (Task 3).
- Produces: `pub fn check_mount_overlaps(fragments: &[LoadedFragment]) -> Result<()>` and `pub fn unattached_mount_notice(manifest: &Manifest, fragments: &[LoadedFragment]) -> Option<String>`, both called from `validate_composition`.

`unattached_mount_notice` covers a case the spec does not name: a composition that carries build mounts but selects no packages has no batched dnf RUN for them to attach to, so nothing is emitted. Silence would hide it, the same reasoning the spec gives for the empty-mount notice, so this reports rather than errors.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/validate.rs`:

```rust
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
        let cases: &[(&[&str], &[&str], bool)] = &[
            // Identical targets.
            (&["etc/pki/entitlement/a.pem"], &["etc/pki/entitlement/b.pem"], true),
            // One target is an ancestor of the other.
            (&["etc/pki/a.pem"], &["etc/pki/tls/mirror/b.pem"], true),
            // Unrelated locations compose fine.
            (&["etc/pki/entitlement/a.pem"], &["etc/rhsm/b.conf"], false),
            // Sharing a textual prefix is not sharing a path.
            (&["etc/pki/a.pem"], &["etc/pkix/b.pem"], false),
        ];

        for (first, second, collides) in cases {
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
                assert!(err.contains("rhel-entitlement"), "names both fragments: {err}");
                assert!(err.contains("internal-mirror"), "names both fragments: {err}");
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
        assert!(err.contains("/etc/pki/rpm-gpg"), "names the written path: {err}");
        assert!(err.contains("repo files"), "names the generator phase: {err}");
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
        let fragments = vec![mount_fragment_at("repo-mount", &["etc/yum.repos.d/internal.repo"])];
        let err = check_mount_overlaps(&fragments)
            .expect_err("that target equals a path the generator writes")
            .to_string();
        assert!(err.contains("/etc/yum.repos.d"), "got: {err}");
    }

    #[test]
    fn mounts_with_no_package_step_produce_a_notice() {
        let fragments = vec![mount_fragment_at("rhel-entitlement", &["etc/rhsm/rhsm.conf"])];
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
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib validate::
```

Expected: `cannot find function check_mount_overlaps in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/validate.rs`, add to the imports at the top:

```rust
use crate::mount::GENERATOR_WRITTEN_PATHS;
```

Extend the entry point:

```rust
pub fn validate_composition(manifest: &Manifest, fragments: &[LoadedFragment]) -> Result<()> {
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
```

Add at the end of the file, before `#[cfg(test)]`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib validate:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/validate.rs
git commit -m "feat(validate): refuse colliding mount targets

Overlap is prefix-based in both directions between fragments, and one
directional against the paths the generator writes before the package step:
a mount over /etc/pki/rpm-gpg would hide the GPG keys for exactly the RUN
that verifies packages against them. First wins on credentials produces
silent authentication mysteries, so both cases refuse.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 8: Emit mount flags on the batched package RUN

**Files:**
- Modify: `src/generator.rs` (the packages section, lines 246 to 294)

**Interfaces:**
- Consumes: `LoadedFragment.mount_points` (Task 3), `MountPoint::layer_source`, `MountPoint::context_source`, `MountPoint::target` (Task 1), `FragmentSource::Registry` (existing).
- Produces: no new signature. Emission behavior consumed by Tasks 10, 15, and the docs task.

This step writes both emission forms at once, because they are one `match` in one place; Task 10 is the self-contained assertions and the no-registry-reference guarantee that go with the second form.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/generator.rs`, after the existing helpers:

```rust
    /// A fragment carrying metadata and `mount/` alone, pinned by digest as
    /// build mounts require.
    fn make_mount_fragment(name: &str, mount_files: &[&str]) -> (LoadedFragment, ManifestFragment) {
        let pinned = format!("quay.io/acme/{}@sha256:d00d", name);
        let files: Vec<PathBuf> = mount_files.iter().map(PathBuf::from).collect();
        let loaded = LoadedFragment {
            fragment: Fragment {
                name: FragmentName::new(name).expect("test fragment name must be valid"),
                version: "1.0".into(),
                description: "test".into(),
                vendor: None,
                provides: FragmentProvides { repos: vec![] },
                packages: FragmentPackages { required: vec![] },
                conflicts: FragmentConflicts { fragments: vec![] },
            },
            tree_paths: vec![],
            hook_paths: vec![],
            source: FragmentSource::Registry {
                image_ref: pinned.clone(),
            },
            resolved_digest: Some("sha256:d00d".into()),
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
            mount_points: crate::mount::derive_mount_points(name, &files)
                .expect("fixture derives mount points"),
        };
        let manifest_frag = ManifestFragment {
            image: pinned,
            packages: vec!["some-package".into()],
            mirror: None,
        };
        (loaded, manifest_frag)
    }

    /// Every `--mount=` option string the output carries, trailing
    /// continuation backslash removed.
    fn mount_flags(output: &str) -> Vec<String> {
        output
            .lines()
            .flat_map(|l| l.split_whitespace())
            .filter(|t| t.starts_with("--mount="))
            .map(|t| t.trim_end_matches('\\').to_string())
            .collect()
    }

    #[test]
    fn build_mounts_attach_to_the_batched_package_run() {
        let (frag, mf) = make_mount_fragment("rhel-entitlement", &["etc/pki/entitlement/cert.pem"]);
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf],
        };
        let output = generate_containerfile(&manifest, &[frag], None, false, false).unwrap();

        assert!(
            output.contains(
                "RUN --mount=type=bind,from=quay.io/acme/rhel-entitlement@sha256:d00d,\
                 source=/fragment/mount/etc/pki/entitlement,\
                 target=/etc/pki/entitlement,ro,z \\"
            ),
            "got:\n{output}"
        );
        let dnf_line = output
            .lines()
            .position(|l| l.trim_start().starts_with("dnf install -y"))
            .expect("the batched install line is still emitted");
        let mount_line = output
            .lines()
            .position(|l| l.contains("--mount=type=bind,from=quay.io/acme"))
            .expect("the mount line is emitted");
        assert!(mount_line < dnf_line, "the mount attaches to that RUN:\n{output}");
    }

    #[test]
    fn one_flag_per_derived_mount_point_in_manifest_order() {
        let (first, mf_first) = make_mount_fragment("entitlement", &["etc/pki/entitlement/c.pem"]);
        let (mut second, mf_second) = make_mount_fragment("rhsm", &["etc/rhsm/rhsm.conf"]);
        second.manifest_index = 1;
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf_first, mf_second],
        };
        let output = generate_containerfile(&manifest, &[first, second], None, false, false).unwrap();

        let flags = mount_flags(&output);
        assert_eq!(flags.len(), 2, "one flag per derived point:\n{output}");
        assert!(flags[0].contains("target=/etc/pki/entitlement"), "{flags:?}");
        assert!(flags[1].contains("target=/etc/rhsm"), "{flags:?}");
    }

    #[test]
    fn build_mounts_are_read_only_and_relabelled() {
        let (frag, mf) = make_mount_fragment("entitlement", &["etc/pki/entitlement/c.pem"]);
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf],
        };
        let output = generate_containerfile(&manifest, &[frag], None, false, false).unwrap();
        let flags = mount_flags(&output);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].ends_with(",ro,z"), "got: {}", flags[0]);
    }

    #[test]
    fn a_composition_without_build_mounts_emits_the_run_line_unchanged() {
        // Byte stability for every existing manifest: no mounts, no change.
        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(&manifest, &[epel], None, false, false).unwrap();
        assert!(output.contains("RUN dnf install -y \\\n"), "got:\n{output}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib generator::tests::build_mounts
```

Expected: the emission assertions fail because no `--mount=type=bind,from=` line appears before `dnf install`.

- [ ] **Step 3: Write the implementation**

In `src/generator.rs`, immediately before `if !all_packages.is_empty() {` (around line 269), insert:

```rust
    // Build mounts. The package phase already copies the config that belongs
    // in the image; this is its second verb, mounting the material that must
    // not persist. One flag per derived mount point, fragments in manifest
    // order, and always inline: a build-mount reference is never a named
    // stage, including under --pin-digests.
    let mut mount_flags: Vec<String> = Vec::new();
    for loaded in fragments {
        let FragmentSource::Registry { ref image_ref } = loaded.source;
        for point in &loaded.mount_points {
            mount_flags.push(if self_contained {
                // No registry reference may appear in this mode's output, so
                // the material is read from the context instead, the same
                // pattern hooks use.
                format!(
                    "--mount=type=bind,source={},target={},ro,z",
                    point.context_source(&loaded.fragment.name),
                    point.target()
                )
            } else {
                format!(
                    "--mount=type=bind,from={},source={},target={},ro,z",
                    image_ref,
                    point.layer_source(),
                    point.target()
                )
            });
        }
    }
```

Then replace the single line:

```rust
        writeln!(out, "RUN dnf install -y \\")?;
```

with:

```rust
        match mount_flags.split_first() {
            // The unmounted form is byte-identical to what every existing
            // manifest already generates.
            None => writeln!(out, "RUN dnf install -y \\")?,
            Some((first, rest)) => {
                writeln!(out, "RUN {} \\", first)?;
                for flag in rest {
                    writeln!(out, "    {} \\", flag)?;
                }
                writeln!(out, "    dnf install -y \\")?;
            }
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib generator:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

Expected: all generator tests pass, including the existing ones, since the no-mount path is unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/generator.rs
git commit -m "feat(generator): mount build material onto the batched package RUN

No new stage, no new RUN, no new layer: the mount attaches to the step it
serves and is gone when that step ends. Emission is always inline, because
a named stage for a build-mount reference would be consumed by nothing.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 9: Exclude pure-mount fragments from the named-stage loop

**Files:**
- Modify: `src/generator.rs` (fragment stages section, lines 130 to 145)

**Interfaces:**
- Consumes: `LoadedFragment::is_pure_mount` (Task 3).
- Produces: no new signature.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/generator.rs`:

```rust
    #[test]
    fn a_pure_mount_fragment_gets_no_named_stage() {
        let (mount_frag, mf_mount) = make_mount_fragment("entitlement", &["etc/rhsm/rhsm.conf"]);
        let (mut epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        epel.manifest_index = 1;
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf_mount, mf_epel],
        };
        let output =
            generate_containerfile(&manifest, &[mount_frag, epel], None, false, false).unwrap();

        assert!(
            !output.contains("AS frag-entitlement"),
            "a stage for a pure mount fragment would be consumed by nothing:\n{output}"
        );
        assert!(
            output.contains("AS frag-epel"),
            "fragments a COPY references still get their stage:\n{output}"
        );
        assert!(
            output.contains("--mount=type=bind,from=quay.io/acme/entitlement@sha256:d00d"),
            "and the mount still references the image inline:\n{output}"
        );
    }

    #[test]
    fn a_composition_of_only_pure_mount_fragments_emits_no_stage_section() {
        let (mount_frag, mf_mount) = make_mount_fragment("entitlement", &["etc/rhsm/rhsm.conf"]);
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf_mount],
        };
        let output = generate_containerfile(&manifest, &[mount_frag], None, false, false).unwrap();
        assert!(
            !output.contains("# --- Fragment stages ---"),
            "an empty section banner is worse than no section:\n{output}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib generator::tests::a_pure_mount_fragment_gets_no_named_stage
```

Expected: the output contains `FROM quay.io/acme/entitlement@sha256:d00d AS frag-entitlement`.

- [ ] **Step 3: Write the implementation**

In `src/generator.rs`, replace the fragment stages block:

```rust
    if use_named_stages && !self_contained {
        if !ocp {
            writeln!(out, "# --- Fragment stages ---")?;
        }
        for loaded in fragments {
            let FragmentSource::Registry { ref image_ref } = loaded.source;
            writeln!(out, "FROM {} AS frag-{}", image_ref, loaded.fragment.name)?;
        }
        if !ocp {
            writeln!(out)?;
        }
    }
```

with:

```rust
    // A fragment consisting of metadata and mount/ alone is excluded: its
    // reference is always emitted inline on the package RUN, so a stage for
    // it would be consumed by nothing and would spend characters against the
    // MachineOSConfig content limit for no reader.
    let staged: Vec<&LoadedFragment> = fragments.iter().filter(|f| !f.is_pure_mount()).collect();
    if use_named_stages && !self_contained && !staged.is_empty() {
        if !ocp {
            writeln!(out, "# --- Fragment stages ---")?;
        }
        for loaded in &staged {
            let FragmentSource::Registry { ref image_ref } = loaded.source;
            writeln!(out, "FROM {} AS frag-{}", image_ref, loaded.fragment.name)?;
        }
        if !ocp {
            writeln!(out)?;
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib generator:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/generator.rs
git commit -m "feat(generator): skip the named stage for pure mount fragments

Under --pin-digests the generator emits a named stage per fragment for
readability. A fragment carrying metadata and mount/ alone has no COPY and
no hook RUN to consume one, so its stage would be dead text against a
4096-character budget.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 10: Self-contained mount emission carries no registry reference

**Files:**
- Modify: `src/generator.rs` (test module only; the emission code landed in Task 8)

**Interfaces:**
- Consumes: emission from Task 8, `MountPoint::context_source` (Task 1).
- Produces: no new signature. This task pins the guarantee that separates the two forms.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/generator.rs`:

```rust
    #[test]
    fn self_contained_mounts_read_from_the_context_and_name_no_registry() {
        let (frag, mf) = make_mount_fragment("rhel-entitlement", &["etc/pki/entitlement/cert.pem"]);
        let manifest = Manifest {
            base: "quay.io/test/base:1".into(),
            source_path: "test-manifest.yaml".into(),
            fragments: vec![mf],
        };
        let output = generate_containerfile(&manifest, &[frag], None, false, true).unwrap();

        assert!(
            output.contains(
                "RUN --mount=type=bind,\
                 source=fragments/rhel-entitlement/mount/etc/pki/entitlement,\
                 target=/etc/pki/entitlement,ro,z \\"
            ),
            "got:\n{output}"
        );
        assert!(
            !output.contains("from="),
            "self-contained output carries no fragment registry reference at all:\n{output}"
        );
        assert!(
            !output.contains("quay.io/acme"),
            "not in a mount, not in a comment:\n{output}"
        );
        assert!(
            !output.contains("sha256:d00d"),
            "the digest pin lives in the manifest, not in this output:\n{output}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib generator::tests::self_contained_mounts_read_from_the_context
```

Expected: it passes if Task 8's `self_contained` branch is correct. If it fails, the failure is in Task 8's branch, and this test is what finds it. Either outcome is a valid gate; do not skip the run.

- [ ] **Step 3: Fix anything the test found**

If the assertion on `from=` fails, check that the `self_contained` branch in Task 8's `mount_flags` loop is the one being taken and that no other emission path prints `image_ref` in this mode.

- [ ] **Step 4: Run the whole suite**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/generator.rs
git commit -m "test(generator): pin the self-contained mount emission form

Self-contained output carries no registry reference anywhere, comments
included, so mount material has to be read from the context. The digest
anchor moves with the reference: it lives in the manifest, and
materialization still pulls digest-verified content.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 11: Materialize `mount/` to disk under an explicit policy

**Files:**
- Modify: `src/loader.rs` (`extract_fragment_payload_to_disk` at line 268, `materialize_fragment` at line 309)
- Modify: `src/self_contained.rs` (`write_output` at line 225, and test call sites)

**Interfaces:**
- Consumes: `MountMaterialization` (Task 1).
- Produces:
  - `pub(crate) fn extract_fragment_payload_to_disk(compressed: &[u8], dest_dir: &Path, mounts: MountMaterialization) -> Result<()>`
  - `pub fn materialize_fragment(image_ref: &str, dest_dir: &Path, mounts: MountMaterialization) -> Result<()>`
  - Both consumed by Task 13.

`write_output` passes `MountMaterialization::Skip` for now so the tree compiles and behavior is unchanged; Task 13 threads the real policy through.

- [ ] **Step 1: Write the failing test**

Add to `mod layer_tests` in `src/loader.rs`:

```rust
    #[test]
    fn mount_material_lands_on_disk_only_under_the_write_policy() {
        use crate::mount::MountMaterialization;

        // (policy, mount file exists on disk afterward)
        let cases = [
            (MountMaterialization::Skip, false),
            (MountMaterialization::Write, true),
        ];
        for (policy, expected) in cases {
            let tarball = create_test_tarball(&[
                ("fragment/tree/etc/motd", b"hello" as &[u8]),
                ("fragment/mount/etc/pki/entitlement/cert.pem", b"secret"),
            ]);
            let workdir = tempfile::tempdir().unwrap();
            extract_fragment_payload_to_disk(&tarball, workdir.path(), policy).unwrap();

            assert!(
                workdir.path().join("tree/etc/motd").is_file(),
                "tree payload is unaffected by the mount policy"
            );
            assert_eq!(
                workdir
                    .path()
                    .join("mount/etc/pki/entitlement/cert.pem")
                    .is_file(),
                expected,
                "policy={policy:?}"
            );
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib loader::layer_tests::mount_material_lands_on_disk
```

Expected: `this function takes 2 arguments but 3 arguments were supplied`.

- [ ] **Step 3: Write the implementation**

In `src/loader.rs`, extend the `crate::mount` import to include `MountMaterialization`:

```rust
use crate::mount::{
    derive_mount_points, empty_mount_notice, MountMaterialization, MountPoint,
    MOUNTS_ANNOTATION_KEY,
};
```

Change `extract_fragment_payload_to_disk`'s signature and destination match:

```rust
pub(crate) fn extract_fragment_payload_to_disk(
    compressed: &[u8],
    dest_dir: &Path,
    mounts: MountMaterialization,
) -> Result<()> {
```

and replace the destination selection:

```rust
        let dest = if let Ok(rel) = path.strip_prefix("fragment/tree") {
            dest_dir.join("tree").join(rel)
        } else if let Ok(rel) = path.strip_prefix("fragment/hooks") {
            dest_dir.join("hooks").join(rel)
        } else {
            continue;
        };
```

with:

```rust
        let dest = if let Ok(rel) = path.strip_prefix("fragment/tree") {
            dest_dir.join("tree").join(rel)
        } else if let Ok(rel) = path.strip_prefix("fragment/hooks") {
            dest_dir.join("hooks").join(rel)
        } else if let Ok(rel) = path.strip_prefix(MOUNT_TAR_PREFIX) {
            // Skipped unless the caller opted in: materializing mount
            // material is a custody change, not a packaging detail, and the
            // default output must not make a durable copy of it.
            match mounts {
                MountMaterialization::Skip => continue,
                MountMaterialization::Write => dest_dir.join("mount").join(rel),
            }
        } else {
            continue;
        };
```

Update the doc comment above the function to mention the new parameter:

```rust
/// Write a layer's `fragment/tree/`, `fragment/hooks/`, and, under
/// [`MountMaterialization::Write`], `fragment/mount/` payload to disk under
/// `dest_dir`. Shares the same tar-entry security validation as the
/// metadata-only extractors above.
```

Change `materialize_fragment`:

```rust
pub fn materialize_fragment(
    image_ref: &str,
    dest_dir: &Path,
    mounts: MountMaterialization,
) -> Result<()> {
    for layer_bytes in pull_layer_bytes(image_ref)? {
        extract_fragment_payload_to_disk(&layer_bytes, dest_dir, mounts)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Update every call site**

In `src/self_contained.rs::write_output`, change the closure argument:

```rust
        |image_ref, dest| crate::loader::materialize_fragment(image_ref, dest, MountMaterialization::Skip),
```

and add the import at the top of the file:

```rust
use crate::mount::MountMaterialization;
```

Then find and fix the test call sites in both files:

```bash
rg -n 'extract_fragment_payload_to_disk\(' src/
```

Every call in a test gets `MountMaterialization::Skip` as its third argument except where the test is specifically about mount material. Expected sites: `src/loader.rs` (four in `mod layer_tests`, one further down), `src/self_contained.rs` (four inside test closures). In `src/self_contained.rs` tests, add `use crate::mount::MountMaterialization;` to `mod tests` if it is not already in scope through the file-level import.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 6: Commit**

```bash
git add src/loader.rs src/self_contained.rs
git commit -m "feat(loader): materialize mount/ only when the caller opts in

Putting credential material durably on disk in a build context is a custody
change, so the extractor takes an explicit policy rather than a default.
Nothing passes Write yet.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 12: Gate self-contained output on `--materialize-mounts`

**Files:**
- Modify: `src/self_contained.rs` (new function at the end, before `#[cfg(test)]`)

**Interfaces:**
- Consumes: `LoadedFragment.mount_points` (Task 3), `MountPoint::context_source` (Task 1), `archive_path_for` (existing private, `src/self_contained.rs:247`).
- Produces: `pub fn check_mount_materialization(dir: &Path, fragments: &[LoadedFragment], materialize_mounts: bool) -> Result<()>`, called from `src/main.rs` in Task 15.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/self_contained.rs`:

```rust
    fn mount_carrying_fragment(name: &str, mount_files: &[&str]) -> LoadedFragment {
        let mut loaded = test_loaded_fragment(name);
        let files: Vec<PathBuf> = mount_files.iter().map(PathBuf::from).collect();
        loaded.mount_points =
            crate::mount::derive_mount_points(name, &files).expect("fixture derives");
        loaded
    }

    #[test]
    fn self_contained_refuses_mount_material_without_the_flag() {
        let fragments = vec![mount_carrying_fragment(
            "rhel-entitlement",
            &["etc/pki/entitlement/cert.pem"],
        )];
        let err = check_mount_materialization(Path::new("out"), &fragments, false)
            .expect_err("materializing credential material is a custody change")
            .to_string();

        assert!(err.contains("rhel-entitlement"), "names the fragment: {err}");
        assert!(
            err.contains("out/fragments/rhel-entitlement/mount/etc/pki/entitlement"),
            "names the exact path that would land on disk: {err}"
        );
        assert!(err.contains("out.tar.gz"), "names the sibling archive: {err}");
        assert!(err.contains("--materialize-mounts"), "names the flag: {err}");
        assert!(
            err.contains("git"),
            "leads with the custody change and the not-for-git warning: {err}"
        );
    }

    #[test]
    fn self_contained_proceeds_with_the_flag() {
        let fragments = vec![mount_carrying_fragment(
            "rhel-entitlement",
            &["etc/pki/entitlement/cert.pem"],
        )];
        assert!(check_mount_materialization(Path::new("out"), &fragments, true).is_ok());
    }

    #[test]
    fn a_composition_without_mount_material_is_never_gated() {
        let fragments = vec![test_loaded_fragment("epel")];
        assert!(check_mount_materialization(Path::new("out"), &fragments, false).is_ok());
    }

    #[test]
    fn the_gate_names_every_offending_fragment_and_path() {
        let fragments = vec![
            mount_carrying_fragment("entitlement", &["etc/pki/entitlement/c.pem"]),
            mount_carrying_fragment("mirror", &["etc/pki/tls/mirror/client.pem"]),
        ];
        let err = check_mount_materialization(Path::new("build/ctx"), &fragments, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("build/ctx/fragments/entitlement/mount/etc/pki/entitlement"), "{err}");
        assert!(err.contains("build/ctx/fragments/mirror/mount/etc/pki/tls/mirror"), "{err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib self_contained::tests::self_contained_refuses_mount_material
```

Expected: `cannot find function check_mount_materialization in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/self_contained.rs`, add before `#[cfg(test)]`:

```rust
/// Refuse to write build-mount material into a self-contained context
/// unless the user asked for it by name.
///
/// Self-contained output carries no registry references by design, so mount
/// material cannot arrive as an inline `from=` and has to be materialized
/// into the context and its sibling archive. That is a custody change rather
/// than a packaging detail, which is why it is gated rather than noticed.
pub fn check_mount_materialization(
    dir: &Path,
    fragments: &[LoadedFragment],
    materialize_mounts: bool,
) -> Result<()> {
    if materialize_mounts {
        return Ok(());
    }

    let mut landing: Vec<String> = Vec::new();
    for loaded in fragments {
        for point in &loaded.mount_points {
            landing.push(format!(
                "  {} (fragment '{}')",
                dir.join(point.context_source(&loaded.fragment.name)).display(),
                loaded.fragment.name
            ));
        }
    }
    if landing.is_empty() {
        return Ok(());
    }

    bail!(
        "--self-contained would write build-mount material into the build context. \
         These paths would land on disk:\n{}\n\
         and the same bytes would be copied into the sibling archive {}. Self-contained \
         output carries no registry references, so mount material cannot arrive by \
         reference and has to be materialized. Treat that as a custody change: the copy \
         is durable, and git does not record file modes, so committing the context would \
         publish owner-only material world-readable. If that is what you intend, re-run \
         with --materialize-mounts, which writes the mount subtrees owner-only and adds a \
         .gitignore covering fragments/*/mount/. Otherwise generate without \
         --self-contained, where the material is mounted from a digest-pinned reference \
         instead of being materialized into the context.",
        landing.join("\n"),
        archive_path_for(dir).display()
    );
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib self_contained:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/self_contained.rs
git commit -m "feat(self-contained): gate mount materialization behind a flag

Self-contained output has no way to reference mount material, so it has to
copy it onto disk. That changes who holds the material and for how long, so
the error leads with the custody change and the exact paths, and never
presents the flag as a routine unblock.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 13: Write materialized mount subtrees owner-only

**Files:**
- Modify: `src/self_contained.rs` (`OUTPUT_DIR_MODE` at line 45, `write_output_with` at line 134, `write_output` at line 225)
- Modify: `src/main.rs` (the single `write_output` call at line 156)

**Interfaces:**
- Consumes: `MountMaterialization` (Task 1), `materialize_fragment` (Task 11).
- Produces:
  - `pub fn write_output(dir: &Path, manifest_path: &Path, containerfile: &str, fragments: &[LoadedFragment], mounts: MountMaterialization) -> Result<()>`
  - private `fn write_output_with(dir, manifest_path, containerfile, fragments, mounts, materialize) -> Result<()>`
  - private `const MOUNT_DIR_MODE: u32` and `fn restrict_mount_tree(fragment_dir: &Path) -> Result<()>`
  - Consumed by Task 14 and Task 15.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/self_contained.rs`:

```rust
    #[test]
    fn materialized_mount_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let layer = build_fixture_layer(&[
            ("fragment/tree/etc/motd", b"hello", 0o644),
            ("fragment/mount/etc/pki/entitlement/cert.pem", b"secret", 0o600),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("entitlement")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();

        let mount_root = dir.join("fragments/entitlement/mount");
        for path in [
            mount_root.clone(),
            mount_root.join("etc"),
            mount_root.join("etc/pki"),
            mount_root.join("etc/pki/entitlement"),
        ] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} must be owner-only, an explicit exception to the output \
                 directory's normally readable handoff contract",
                path.display()
            );
        }

        // The exception is scoped to mount/: everything else keeps the
        // normal handoff mode.
        let tree_mode = fs::metadata(dir.join("fragments/entitlement/tree"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_ne!(tree_mode, 0o700, "tree/ is not credential material");
    }

    #[test]
    fn the_archive_preserves_the_owner_only_mount_modes() {
        let layer = build_fixture_layer(&[(
            "fragment/mount/etc/pki/entitlement/cert.pem",
            b"secret",
            0o600,
        )]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("entitlement")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();
        let archive = create_archive(&dir).unwrap();

        let bytes = fs::read(&archive).unwrap();
        let decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut tar = tar::Archive::new(decoder);
        let mut checked = 0;
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            if path.contains("/mount") && entry.header().entry_type().is_dir() {
                assert_eq!(
                    entry.header().mode().unwrap() & 0o777,
                    0o700,
                    "archived directory {path} must carry the owner-only mode"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "the archive must contain the mount directories");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib self_contained::tests::materialized_mount_directories_are_owner_only
```

Expected: `this function takes 5 arguments but 6 arguments were supplied`.

- [ ] **Step 3: Write the implementation**

In `src/self_contained.rs`, add next to `OUTPUT_DIR_MODE` (line 45):

```rust
/// Permission mode applied to every directory in a materialized `mount/`
/// subtree: owner only. An explicit exception to `OUTPUT_DIR_MODE`'s
/// normally readable handoff contract, because mount material is credential
/// material more often than not and the context and its archive are a
/// durable copy of it.
const MOUNT_DIR_MODE: u32 = 0o700;
```

Add the helper below `check_target_not_symlink`:

```rust
/// Set every directory in `<fragment_dir>/mount` to [`MOUNT_DIR_MODE`].
///
/// A no-op when the fragment carries no mount material. Each directory is
/// read before it is restricted, so tightening a parent never blocks the
/// walk into its children.
#[cfg(unix)]
fn restrict_mount_tree(fragment_dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mount_root = fragment_dir.join("mount");
    if !mount_root.is_dir() {
        return Ok(());
    }
    let mut stack = vec![mount_root];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(MOUNT_DIR_MODE))
            .with_context(|| format!("restricting permissions on {}", dir.display()))?;
    }
    Ok(())
}

/// File modes are a Unix concept; elsewhere the materialized tree carries
/// whatever the platform gives it.
#[cfg(not(unix))]
fn restrict_mount_tree(_fragment_dir: &Path) -> Result<()> {
    Ok(())
}
```

Change `write_output_with`'s signature to take the policy:

```rust
fn write_output_with(
    dir: &Path,
    manifest_path: &Path,
    containerfile: &str,
    fragments: &[LoadedFragment],
    mounts: MountMaterialization,
    materialize: impl Fn(&str, &Path) -> Result<()>,
) -> Result<()> {
```

and extend the per-fragment loop body:

```rust
    for loaded in fragments {
        let FragmentSource::Registry { ref image_ref } = loaded.source;
        let dest = staged_fragments.join(&loaded.fragment.name);
        materialize(image_ref, &dest)
            .with_context(|| format!("materializing fragment '{}'", loaded.fragment.name))?;
        if mounts == MountMaterialization::Write {
            restrict_mount_tree(&dest).with_context(|| {
                format!(
                    "restricting mount material for fragment '{}'",
                    loaded.fragment.name
                )
            })?;
        }
    }
```

Change `write_output`:

```rust
pub fn write_output(
    dir: &Path,
    manifest_path: &Path,
    containerfile: &str,
    fragments: &[LoadedFragment],
    mounts: MountMaterialization,
) -> Result<()> {
    write_output_with(
        dir,
        manifest_path,
        containerfile,
        fragments,
        mounts,
        |image_ref, dest| crate::loader::materialize_fragment(image_ref, dest, mounts),
    )
}
```

- [ ] **Step 4: Update every call site**

In `src/main.rs` line 156, change:

```rust
                write_output(dir, &cli.manifest, &containerfile, &fragments)?;
```

to:

```rust
                write_output(
                    dir,
                    &cli.manifest,
                    &containerfile,
                    &fragments,
                    MountMaterialization::Skip,
                )?;
```

and add to the imports:

```rust
use osfragment_assemble::mount::MountMaterialization;
```

(Task 15 replaces `Skip` with the flag-derived policy.)

Then add `MountMaterialization::Skip,` as the fifth argument, immediately before the closure, at every remaining `write_output_with(` call in the test module:

```bash
rg -n 'write_output_with\(' src/self_contained.rs
```

Expected: about twelve test call sites plus the one in `write_output`.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 6: Commit**

```bash
git add src/self_contained.rs src/main.rs
git commit -m "feat(self-contained): write materialized mount subtrees owner-only

The output directory is a handoff artifact and is normally readable, which
is exactly wrong for credential material. The exception is scoped to
mount/, and the sibling archive carries the same modes because tar records
what it finds on disk.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 14: Emit a `.gitignore` covering the mount subtrees

**Files:**
- Modify: `src/self_contained.rs` (`TOOL_GENERATED_ENTRIES` at line 33, `write_output_with`)

**Interfaces:**
- Consumes: `MountMaterialization` (Task 1), `write_output_with` (Task 13).
- Produces: private `const GITIGNORE_FILENAME: &str` and `fn mount_gitignore_contents() -> &'static str`, plus a widened `TOOL_GENERATED_ENTRIES`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/self_contained.rs`:

```rust
    #[test]
    fn materializing_mounts_writes_a_gitignore_that_explains_itself() {
        let layer = build_fixture_layer(&[(
            "fragment/mount/etc/pki/entitlement/cert.pem",
            b"secret",
            0o600,
        )]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("entitlement")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();

        let ignore = fs::read_to_string(dir.join(".gitignore")).expect("a .gitignore is written");
        assert!(ignore.contains("fragments/*/mount/"), "got: {ignore}");
        assert!(
            ignore.contains("modes"),
            "the comment explains why, so a later failing build reads as policy \
             rather than mystery: {ignore}"
        );
    }

    #[test]
    fn a_run_without_mount_material_writes_no_gitignore() {
        let layer = build_fixture_layer(&[("fragment/tree/etc/motd", b"hello", 0o644)]);
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        let manifest_path = tmp.path().join("manifest.yaml");
        fs::write(&manifest_path, "base: x\n").unwrap();
        let fragments = vec![test_loaded_fragment("epel")];

        write_output_with(
            &dir,
            &manifest_path,
            "FROM x\n",
            &fragments,
            MountMaterialization::Write,
            |_r, dest| {
                crate::loader::extract_fragment_payload_to_disk(
                    &layer,
                    dest,
                    MountMaterialization::Write,
                )
            },
        )
        .unwrap();

        assert!(
            !dir.join(".gitignore").exists(),
            "a context holding no mount material is committable, and an ignore \
             file claiming otherwise would be a lie"
        );
    }

    #[test]
    fn a_context_carrying_a_gitignore_is_still_regenerable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prior-run");
        fs::create_dir_all(dir.join("fragments/entitlement")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        fs::write(dir.join("manifest.yaml"), "base: x\n").unwrap();
        fs::write(dir.join(SENTINEL_FILENAME), sentinel_contents()).unwrap();
        fs::write(dir.join(".gitignore"), mount_gitignore_contents()).unwrap();
        assert!(
            check_target_dir_safe(&dir).is_ok(),
            "the tool writes this file, so it must recognize it on the next run"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib self_contained::tests::materializing_mounts_writes_a_gitignore
```

Expected: `cannot find function mount_gitignore_contents in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/self_contained.rs`, add below `SENTINEL_FILENAME` (line 21):

```rust
/// Filename of the ignore file `--materialize-mounts` writes into a context
/// that holds mount material.
const GITIGNORE_FILENAME: &str = ".gitignore";

/// Contents of that file: the pattern plus the reason it is there.
///
/// Git does not record file modes, so the owner-only protection applied to
/// the mount subtrees does not survive a commit. The consequence is
/// deliberate: a committed context omits its mount material and fails loudly
/// at build time, at the mount source, and this comment is what makes that
/// failure read as policy rather than mystery.
fn mount_gitignore_contents() -> &'static str {
    "# Written by osfragment-assemble.\n\
     #\n\
     # This build context holds build-mount material under fragments/*/mount/,\n\
     # written owner-only on disk. Git does not record file modes, so committing\n\
     # it would publish that material world-readable. While it holds mount\n\
     # material this context is for direct handoff, not for committing.\n\
     #\n\
     # A committed context omits the material, and the build then fails at the\n\
     # mount source rather than building without it.\n\
     fragments/*/mount/\n"
}
```

Widen the recognized entry set:

```rust
const TOOL_GENERATED_ENTRIES: &[&str] = &[
    "Containerfile",
    "manifest.yaml",
    "fragments",
    SENTINEL_FILENAME,
    GITIGNORE_FILENAME,
];
```

In `write_output_with`, after the per-fragment materialization loop and before the permissions normalization block, add:

```rust
    // Only when the context actually holds mount material: an ignore file
    // announcing credential material in a context that has none would be a
    // lie, and it would make an ordinary context look uncommittable.
    let holds_mount_material = mounts == MountMaterialization::Write
        && fragments.iter().any(|f| !f.mount_points.is_empty());
    if holds_mount_material {
        fs::write(
            staging.path().join(GITIGNORE_FILENAME),
            mount_gitignore_contents(),
        )
        .context("writing the mount material .gitignore")?;
    }
```

Note for the tests above: the `.gitignore` is gated on a fragment actually carrying mount points, and `test_loaded_fragment` carries none. So in `materializing_mounts_writes_a_gitignore_that_explains_itself`, replace `test_loaded_fragment("entitlement")` with `mount_carrying_fragment("entitlement", &["etc/pki/entitlement/cert.pem"])`, the helper added in Task 12. Leave the negative test on `test_loaded_fragment("epel")`, which is exactly the mountless case it checks. Task 13's tests need no change: `restrict_mount_tree` is gated on the policy alone, so it runs over whatever the materialization actually wrote.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib self_contained:: && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/self_contained.rs
git commit -m "feat(self-contained): ignore materialized mount subtrees in git

Git does not record file modes, so the owner-only protection does not
survive a commit. The ignore file carries the reason, so the build failure
a committed context produces reads as policy rather than mystery.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 15: Wire `--materialize-mounts` through the CLI

**Files:**
- Modify: `src/main.rs` (`Cli` struct at line 26, the self-contained branch at line 129)
- Modify: `tests/cli.rs` (add cases at the end)

**Interfaces:**
- Consumes: `check_mount_materialization` (Task 12), `write_output` with the policy parameter (Task 13), `MountMaterialization::from_flag` (Task 1).
- Produces: the `--materialize-mounts` flag on the assembly path.

- [ ] **Step 1: Write the failing test**

Add to `tests/cli.rs`:

```rust
#[test]
fn materialize_mounts_requires_self_contained() {
    // The flag only means anything for the output mode that has to copy
    // mount material onto disk, so passing it alone must not be accepted
    // and then silently ignored.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--materialize-mounts"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--self-contained"));
}

#[test]
fn materialize_mounts_is_accepted_alongside_self_contained() {
    // Fails for an unrelated reason (no manifest in the test's cwd), which
    // is the point: it must not fail on argument parsing.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", "out", "--materialize-mounts"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("reading manifest")
                .or(predicate::str::contains("was not generated by this tool")),
        );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --test cli materialize_mounts
```

Expected: `unexpected argument '--materialize-mounts' found`.

- [ ] **Step 3: Add the flag**

In `src/main.rs`, add to the `Cli` struct after `self_contained`:

```rust
    /// Write build-mount material into the --self-contained build context.
    /// Mount material is credential material more often than not, so the
    /// context and its archive become a durable copy of it: the mount
    /// subtrees are written owner-only and a .gitignore keeps the context
    /// out of git while it holds them.
    #[arg(long, requires = "self_contained")]
    materialize_mounts: bool,
```

- [ ] **Step 4: Wire the gate and the policy**

In `src/main.rs`, extend the import line 14:

```rust
use osfragment_assemble::self_contained::{
    check_mount_materialization, check_target_dir_safe, create_archive, write_output,
};
```

In the `if let Some(dir) = &cli.self_contained {` branch, insert before `generate_containerfile`:

```rust
                check_mount_materialization(dir, &fragments, cli.materialize_mounts)?;
```

and change the `write_output` call's fifth argument from `MountMaterialization::Skip` to:

```rust
                    MountMaterialization::from_flag(cli.materialize_mounts),
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 6: Commit**

```bash
git add src/main.rs tests/cli.rs
git commit -m "feat(cli): add --materialize-mounts

The flag is meaningless outside --self-contained, so clap requires it
rather than accepting and ignoring it. The gate runs before generation, on
the same side of the pipeline as the target directory check.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 16: `inspect` renders a `mount/` section

**Files:**
- Modify: `src/inspect.rs` (`run_inspect` at line 7)
- Modify: `tests/cli.rs`

**Interfaces:**
- Consumes: `derive_mount_points`, `empty_mount_notice`, `MountPoint::target`, `MOUNT_SECTION_NOTE` (Task 1); `LoadedFragment.mount_points` (Task 3); `collect_display_paths` (existing private, `src/inspect.rs:97`).
- Produces: no new signature.

The local form is the pre-publish confirmation surface: run against a fragment directory it shows the targets generation will derive, and it reaches no registry.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/inspect.rs`. If that module has no fixtures yet, this is the first, so create the fragment tree the test needs inline:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const MOUNT_FRAGMENT_TOML: &str = r#"
[fragment]
name = "rhel-entitlement"
version = "1.0"
description = "RHEL entitlement certificates for the package step"
"#;

    fn write_mount_fragment(dir: &Path) {
        std::fs::write(dir.join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();
        let target = dir.join("mount/etc/pki/entitlement");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("cert.pem"), b"cert").unwrap();
        std::fs::write(target.join("key.pem"), b"key").unwrap();
    }

    #[test]
    fn local_inspect_derives_the_targets_generation_will_use() {
        let tmp = tempfile::tempdir().unwrap();
        write_mount_fragment(tmp.path());

        let section = local_mount_section(tmp.path(), "rhel-entitlement")
            .expect("a fragment directory with mount/ content derives targets");
        assert_eq!(
            section.targets,
            vec!["/etc/pki/entitlement".to_string()],
            "two files in one directory derive one target"
        );
        assert!(section.notice.is_none());
    }

    #[test]
    fn local_inspect_notices_an_empty_mount_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();
        std::fs::create_dir_all(tmp.path().join("mount")).unwrap();

        let section = local_mount_section(tmp.path(), "rhel-entitlement").unwrap();
        assert!(section.targets.is_empty());
        assert!(
            section.notice.is_some(),
            "an empty mount/ is almost always an authoring mistake"
        );
    }

    #[test]
    fn a_fragment_without_mount_has_no_section() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("fragment.toml"), MOUNT_FRAGMENT_TOML).unwrap();

        let section = local_mount_section(tmp.path(), "rhel-entitlement").unwrap();
        assert!(section.targets.is_empty());
        assert!(section.notice.is_none());
    }
}
```

Also add to `tests/cli.rs`:

```rust
#[test]
fn inspect_without_mounts_prints_no_mount_section() {
    // Every shipped example fragment is mountless, so the section must be
    // absent rather than rendered empty.
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["inspect", "examples/fragments/epel"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mount/").not());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib inspect::
```

Expected: `cannot find function local_mount_section in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/inspect.rs`, add to the imports:

```rust
use crate::mount::{derive_mount_points, empty_mount_notice, MountPoint, MOUNT_SECTION_NOTE};
```

Add the section builder and its type below `local_entrypoint_mode`:

```rust
/// The `mount/` section for one fragment: the derived targets, plus the
/// notice for a `mount/` directory that holds no files.
struct MountSection {
    targets: Vec<String>,
    notice: Option<String>,
}

/// Build the section from a local fragment directory, reaching no registry.
///
/// This is the pre-publish confirmation surface: run against the fragment
/// directory it shows the targets generation will derive, before the
/// fragment is published anywhere.
fn local_mount_section(dir: &Path, fragment_name: &str) -> Result<MountSection> {
    let mut files = Vec::new();
    collect_display_paths(dir, "mount", &mut files)?;
    let files: Vec<std::path::PathBuf> = files.iter().map(std::path::PathBuf::from).collect();

    let derived = derive_mount_points(fragment_name, &files)?;
    let notice = empty_mount_notice(fragment_name, dir.join("mount").is_dir(), &derived);
    Ok(MountSection {
        targets: derived.iter().map(MountPoint::target).collect(),
        notice,
    })
}

/// Print the section, or nothing when the fragment carries no mounts.
fn print_mount_section(section: &MountSection) {
    if let Some(notice) = &section.notice {
        eprintln!("{}", notice);
    }
    if section.targets.is_empty() {
        return;
    }
    println!();
    println!("mount/");
    for target in &section.targets {
        println!("  {}", target);
    }
    println!("{}", MOUNT_SECTION_NOTE);
}
```

In `run_inspect`, change the destructuring so both branches produce a section. The local branch:

```rust
    let (fragment, tree_paths, hook_paths, mount_section) = if path.is_dir() {
        let toml_path = path.join("fragment.toml");
        let content = std::fs::read_to_string(&toml_path)?;
        let frag = parse_fragment_toml(&content)?;

        let mut paths = Vec::new();
        collect_display_paths(path, "tree", &mut paths)?;

        // Hook files count at any depth, so this scan is recursive: a
        // fragment whose hooks/ holds only lib/helper.sh still needs an
        // entrypoint, and a shallow scan would pass it.
        let mut hook_list = Vec::new();
        collect_display_paths(path, "hooks", &mut hook_list)?;
        if !hook_list.is_empty() {
            validate_hooks_entrypoint(frag.name.as_str(), local_entrypoint_mode(path))?;
        }

        let mount_section = local_mount_section(path, frag.name.as_str())?;
        (frag, paths, hook_list, mount_section)
    } else {
```

and the registry branch's tail:

```rust
        let mount_section = MountSection {
            targets: loaded.mount_points.iter().map(MountPoint::target).collect(),
            notice: None,
        };
        (loaded.fragment, display_paths, hook_list, mount_section)
    };
```

The registry branch carries no notice because `load_registry_fragment` already printed one at load if there was anything to say.

Finally, call the printer between the `tree/` block and the `hooks/` block, matching the layout order where `mount/` is a sibling of both:

```rust
    print_mount_section(&mount_section);
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/inspect.rs tests/cli.rs
git commit -m "feat(inspect): render the derived mount targets

The local form closes the author-time feedback gap: it shows what
generation will derive before the fragment is published, and it reads the
directory rather than reaching a registry. Against a registry reference
inspect pulls anyway, because its contract is to show payload contents.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 17: `list` reports annotated mount targets

**Files:**
- Modify: `src/list.rs` (`run_list` at line 6)

**Interfaces:**
- Consumes: `LoadedFragment.mount_points` (Tasks 3 and 4), `MountPoint::target` and `MOUNT_SECTION_NOTE` (Task 1).
- Produces: no new signature.

`src/list.rs` has no test module today, so this task adds one over a rendering helper rather than over `run_list`'s stdout.

- [ ] **Step 1: Write the failing test**

Add to the end of `src/list.rs`:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib list::
```

Expected: `cannot find function mount_line in this scope`.

- [ ] **Step 3: Write the implementation**

In `src/list.rs`, add to the imports:

```rust
use crate::mount::{MountPoint, MOUNT_SECTION_NOTE};
```

Add above `run_list`:

```rust
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
```

Inside the `for loaded in fragments` loop, after the row is printed (both branches of the `if has_digests`), add:

```rust
        if let Some(line) = mount_line(&loaded.mount_points) {
            println!("{}", line);
        }
```

After `println!("{} fragments", fragments.len());`, add:

```rust
    // Only when something in the manifest mounts: the note explains a line
    // the reader just saw, and means nothing without it. Reading these from
    // the mounts annotation is what lets this run without pulling layers.
    if fragments.iter().any(|f| !f.mount_points.is_empty()) {
        println!("{}", MOUNT_SECTION_NOTE);
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
```

- [ ] **Step 5: Commit**

```bash
git add src/list.rs
git commit -m "feat(list): report which fragments mount material

With the mounts annotation present this comes from registry metadata alone,
which is the whole point of annotating it. Without the annotation the
metadata-only path already falls back to a full pull, as it does for any
other missing annotation.

Assisted-by: Claude Code (Opus 5)"
```

---

### Task 18: Full verification

**Files:**
- Modify: none

**Interfaces:**
- Consumes: everything above.
- Produces: nothing.

- [ ] **Step 1: Run the format gate**

```bash
cargo fmt --check
```

Expected: no output, exit status 0.

- [ ] **Step 2: Run the lint gate**

```bash
cargo clippy --all-targets -- -D clippy::all
```

Expected: `Finished` with zero warnings.

- [ ] **Step 3: Run the whole suite**

```bash
cargo test
```

Expected: every unit test and every `tests/cli.rs` test passes. The suite is offline; a test that needed a registry would fail in CI, so nothing added here may shell out to skopeo.

- [ ] **Step 4: Confirm the existing examples still generate byte-identically where they should**

```bash
cargo run -- inspect examples/fragments/epel
cargo run -- inspect examples/fragments/tailscale
```

Expected: no `mount/` section on either, and no notice on stderr. Every shipped example fragment is mountless, so this is the regression check that mountless output is unchanged.

- [ ] **Step 5: Grep for the two content rules**

```bash
rg -n '—' src/ tests/ docs/ || echo "no em dashes"
```

Expected: no hits in anything this plan touched. Pre-existing hits elsewhere in `docs/` and `src/` are out of scope for this plan; do not fix them here.

- [ ] **Step 6: Commit anything the gates changed**

If steps 1 through 3 required fixes, commit them:

```bash
git add -A
git commit -m "chore: satisfy the format and lint gates

Assisted-by: Claude Code (Opus 5)"
```

If nothing changed, skip this step.

---

### Task 19: Documentation, changelog, and skill file

**Files:**
- Modify: `docs/fragment-format.md` (Fragment Image Anatomy at line 5, a new section after `tree/` Directory Layout at line 52, Containerfile.fragment Build Pattern at line 105, OCI Annotations at line 126)
- Modify: `docs/design.md` (after "The mechanism" at line 25, and inside "Powerful because of what it refuses to own" at line 41)
- Modify: `README.md` (CLI flags list at line 171)
- Modify: `CHANGELOG.md` (`## [Unreleased]`, `### Added`)
- Modify: `process-docs/skills/codebase-layout.md` and `process-docs/skills/index.md`

**Interfaces:**
- Consumes: the behavior every task above delivered.
- Produces: nothing in code.

Voice rules apply to all of it: no em dashes, and avoid the word "shape."

- [ ] **Step 1: Document the directory in the format spec**

In `docs/fragment-format.md`, update the anatomy block to:

```
/fragment/
├── fragment.toml      # Required: metadata and package declarations
├── tree/              # Optional: files to overlay into target image
│   ├── etc/yum.repos.d/*.repo
│   ├── etc/pki/rpm-gpg/RPM-GPG-KEY-*
│   └── ...            # Arbitrary filesystem paths
├── mount/             # Optional: material bind-mounted during the package step
│   └── etc/pki/entitlement/*.pem
└── hooks/             # Optional: build-time setup (any language)
    ├── entrypoint     # Required when hooks/ has content; the only file run
    └── lib/helper.sh  # Support material, never invoked by the tool
```

Add a `## `mount/` Directory Layout` section immediately after the `tree/` section, covering: the subtree mirrors target paths exactly as `tree/` does; detection is presence-based with no `fragment.toml` section; derivation collects every directory that directly contains a file and drops any nested inside another, so `mount/etc/rhsm/rhsm.conf` plus `mount/etc/rhsm/ca/cert.pem` yields one mount of `/etc/rhsm`; a regular file directly under `mount/` is a generation error; an empty `mount/` produces a notice; the emitted form is

```dockerfile
RUN --mount=type=bind,from=<fragment>@sha256:...,source=/fragment/mount/etc/pki/entitlement,target=/etc/pki/entitlement,ro,z \
    dnf install -y \
        some-package \
    && dnf clean all
```

with the self-contained variant reading `source=fragments/<name>/mount/<path>` and no `from=`; the manifest entry for a fragment carrying `mount/` must be pinned by digest; two fragments mounting colliding targets is an error, as is a target that equals or contains `/etc/yum.repos.d` or `/etc/pki/rpm-gpg`; symlinks and hardlinks are rejected in fragment layers, `mount/` included; the builder never commits the mount source, which is a persistence guarantee and not a confidentiality one, since anything running in that RUN can read the mounted paths.

Add `COPY mount/ /fragment/mount/` to the Containerfile.fragment build pattern block, with the same "omit the line if the fragment has no such directory" note the existing text carries.

Add to the annotation key list:

```
- `com.github.marrusl.osfragment.mounts`: JSON array of mount target paths (e.g., `["/etc/pki/entitlement"]`)
```

and note that this one has no `fragment.toml` counterpart: its authority is the derived targets, so generation cross-checks it whenever it pulls the layer and warns on drift, with layer content winning. Add the author recipe: run `inspect` on the local fragment directory to see the derived targets, then pass them as `--annotation` on your own `podman build`.

- [ ] **Step 2: Document the flag in the README**

In `README.md`, add after the `--self-contained` bullet:

```markdown
- `--materialize-mounts`: With `--self-contained`, write fragment `mount/` material into the build context. Requires `--self-contained`. Without it, a composition carrying `mount/` refuses to generate self-contained output, because the material would land durably on disk in the context and its tarball. With it, the mount subtrees are written owner-only (directories at 0700), the tarball preserves those modes, and a `.gitignore` covering `fragments/*/mount/` is written into the context: git does not record file modes, so a committed context would publish the material world-readable.
```

- [ ] **Step 3: Add the design material**

In `docs/design.md`, add to "The mechanism", after the `hooks/` paragraph:

```markdown
`mount/` is build input that must not persist. Its subtree mirrors target paths the same way `tree/` does, but the files are bind-mounted onto the package install step rather than copied into the image: material at `mount/etc/pki/entitlement/cert.pem` is readable at `/etc/pki/entitlement` while packages install, and the builder commits none of it. The case it exists for is package acquisition that has to authenticate: entitlement certificates, mirror client certificates, CA bundles for a TLS-intercepting proxy. Today that works through host-coupled engine magic, which only works on the right host, or through per-build secret plumbing, which is different for every builder and cannot be shipped by a second author. An artifact-coupled declared mount is pinnable, versionable, and composable like everything else here, and a fragment that must be pinned by digest is one whose trust material cannot be swapped by moving a tag.
```

Add to "Powerful because of what it refuses to own", as a closing paragraph of that section:

```markdown
Build mounts inherit the same boundary. They exist so package acquisition can authenticate, and that is the whole of the claim: the mechanism is not a secrets manager and takes no position on key custody. Signing tools own signing, and users who need signed artifacts sign downstream, in builds they own.
```

- [ ] **Step 4: Add the changelog entry**

In `CHANGELOG.md`, under `## [Unreleased]` and `### Added`, add as the first bullet:

```markdown
- **Build mounts (`mount/`)** - A fragment may carry a `mount/` directory whose subtree mirrors target paths, like `tree/` does. Its material is bind-mounted onto the batched package install step and never committed by the builder, so package acquisition can authenticate from any build host: entitlement certificates, mirror client certificates, CA bundles for a TLS-intercepting proxy. Detection is presence-based, with no `fragment.toml` section and no new fragment kind. One `--mount` flag is emitted per derived mount point, always inline and never as a named stage, and a manifest entry for a fragment carrying `mount/` must be pinned by digest: a movable tag on an artifact that injects trust material into the package step is an invisible substitution point. Colliding mount targets, and targets that would hide the repo files or GPG keys the generator writes ahead of the install, fail generation. `inspect` renders the derived targets for a local fragment directory and for a registry image, and `list` reports them from the new `com.github.marrusl.osfragment.mounts` annotation without pulling layers.

- **`--materialize-mounts`** - With `--self-contained`, writes fragment `mount/` material into the build context. Without it, a composition carrying `mount/` refuses to generate self-contained output, because that output carries no registry references and the material would have to land durably on disk in the context and its tarball. With it, the mount subtrees are written owner-only, the tarball preserves those modes, and a `.gitignore` covering `fragments/*/mount/` is written into the context: git does not record file modes, so a committed context would publish the material world-readable, and omitting it makes the build fail at the mount source instead.
```

Add under `### Fixed`:

```markdown
- **Digest pinning no longer doubles an existing digest** - A manifest entry already written as `registry/repo@sha256:...` produced `registry/repo@sha256:...@sha256:...` in the emitted `FROM` lines under `--pin-digests` and `--self-contained`. Build mounts require the manifest to pin, so this stops being reachable only by manifests that pinned voluntarily.
```

- [ ] **Step 5: Update the skill files**

In `process-docs/skills/codebase-layout.md`, add `src/mount.rs` to the source module table:

```markdown
| `src/mount.rs` | Build mounts: the `MountPoint` newtype, the derivation from a fragment's `mount/` file paths to bind mount points, the render forms each surface needs, the mounts annotation key, and the notice and warning text functions. Consumed by `loader.rs`, `validate.rs`, `generator.rs`, `self_contained.rs`, `inspect.rs`, and `list.rs` |
```

Add `--materialize-mounts` to the flags table, and add `mount/` to the self-contained build context description in the Emitted output section, noting that it appears only under `--materialize-mounts` and that its directories are 0700 rather than the 0755 the rest of the tree carries.

In `process-docs/skills/index.md`, extend the `codebase-layout.md` entry's description to mention the mount module, since the index is what makes a skill visible to future sessions.

- [ ] **Step 6: Verify the docs build nothing and break nothing**

```bash
cargo test && cargo fmt --check && cargo clippy --all-targets -- -D clippy::all
rg -n '—' docs/fragment-format.md docs/design.md README.md CHANGELOG.md process-docs/skills/ || echo "no em dashes in touched docs"
```

- [ ] **Step 7: Commit**

```bash
git add docs/ README.md CHANGELOG.md process-docs/skills/
git commit -m "docs: document build mounts

The format spec gains the mount/ directory, its derivation rule, and the
mounts annotation. The design doc gains the mechanism and the boundary that
comes with it: build mounts authenticate package acquisition, and key
custody is not this tool's job.

Assisted-by: Claude Code (Opus 5)"
```

---

## Self-Review

### 1. Spec coverage

| Spec section | Task |
|---|---|
| Mechanism: `mount/` as a sibling of `tree/` and `hooks/`, presence-based detection, no new `fragment.toml` section | 3, 19 |
| Fragment layout: pure `mount/` fragment valid, hooks entrypoint contract untouched | 3 (`is_pure_mount`), 9 |
| Generated Containerfile: one `--mount` per point on the batched dnf RUN, `ro,z`, inline `from=`, no new stage or layer | 8 |
| Mount point derivation: directories directly containing files, ancestor pruning | 1 |
| Derivation edge: regular file directly under `mount/` is a generation error | 1, 3 |
| Derivation edge: empty `mount/` derives nothing and produces a notice | 1, 3, 16 |
| Self-contained: fail closed naming fragments, paths, and `--materialize-mounts` | 12, 15 |
| Self-contained: opted-in materialization writes `mount/` owner-only at 0700 | 11, 13 |
| Self-contained: the sibling tar.gz preserves those modes | 13 |
| Self-contained: `.gitignore` covering `fragments/*/mount/` with the explanatory comment | 14 |
| Self-contained: the digest anchor lives in the manifest, not the output | 10 |
| Digest pinning: unpinned build-mount reference is a generation error with the corrected literal and how to obtain a digest | 6 |
| Digest pinning: pure-mount fragments excluded from the named-stage loop | 9 |
| Validation: prefix-based overlap between fragments | 7 |
| Validation: prefix rule against generator-written paths, naming the phase | 7 |
| Validation: error messages follow the loader's name-rule-fix pattern | 1, 6, 7, 12 |
| Validation: symlink and hardlink rejection is existing enforcement | 3 (test), 19 (docs) |
| Validation: no other path policy | no task, deliberately |
| Visibility: mounts annotation, hand-authored, `list` no-pull benefit | 4, 17, 19 |
| Visibility: `inspect` renders a `mount/` section for local and registry, local is registry-free | 16 |
| Visibility: cross-check on any layer pull, warn on drift, layer authoritative | 5 |
| Security posture, OCP path, Scope | 19 (docs); no code |

Gaps found and closed while writing this plan:

1. **Doubled digest.** `split_image_ref` returns a digest-bearing reference whole, so both loader sites appended a second digest to an already-pinned manifest entry. Build mounts require pinning, which makes that the normal input. Closed by Task 2.
2. **Empty `mount/` is invisible in a file-path list.** The layer walk collected regular files only, so a `mount/` holding nothing left no trace to notice. Closed by the `has_mount_dir` field in Task 3.
3. **`.gitignore` versus the regenerate check.** `check_target_dir_safe` refuses any entry outside its known set, so writing a `.gitignore` would have made the next run refuse its own output. Closed by widening `TOOL_GENERATED_ENTRIES` in Task 14.
4. **Build mounts with no package step.** The spec attaches mounts to the batched dnf RUN, and that RUN is emitted only when something is installed. A composition with mounts and no packages would silently emit none. Closed by `unattached_mount_notice` in Task 7, which reports rather than refuses. This is the one behavior here the spec does not state.

### 2. Placeholder scan

No occurrence of `TBD`, `TODO`, `add appropriate error handling`, `write tests for the above`, or `similar to Task N`. Every code step carries the code it asks for, and every repeated fixture is written out rather than referenced.

### 3. Type consistency

- `MountPoint`, `derive_mount_points`, `empty_mount_notice`, `mount_annotation_drift`, `MountMaterialization`, `MOUNTS_ANNOTATION_KEY`, `MOUNT_SECTION_NOTE`, `GENERATOR_WRITTEN_PATHS` are all defined in Task 1 and used with those exact names in Tasks 3 through 17.
- `pin_to_digest` is defined in Task 2 and used only in Task 2.
- `LoadedFragment.mount_points` and `LoadedFragment::is_pure_mount` are defined in Task 3 and used in Tasks 4, 6, 7, 8, 9, 12, 13, 14, 16, 17.
- `fetch_annotations` and `mounts_from_annotations` are defined in Task 4 and used in Task 5.
- `check_mount_digest_pins`, `check_mount_overlaps`, and `unattached_mount_notice` are defined in Tasks 6 and 7 and called only from `validate_composition`.
- `extract_fragment_payload_to_disk` and `materialize_fragment` gain their third parameter in Task 11; every caller is updated in the same task, and `write_output` passes `Skip` until Task 13 threads the real policy.
- `write_output` and `write_output_with` gain the `mounts` parameter in Task 13; `src/main.rs` is updated in the same task with a placeholder value that Task 15 replaces.
- `check_mount_materialization` is defined in Task 12 and called in Task 15.
- `local_mount_section`, `MountSection`, and `print_mount_section` are defined and used within Task 16. `mount_line` is defined and used within Task 17.
