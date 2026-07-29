# Self-Contained Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--self-contained <DIR>` to the existing command: it materializes fragment tree/hooks payload into a local build context next to a generated Containerfile that references no registry image except the base, then packages the directory as a sibling `.tar.gz`.

**Architecture:** A new module, `src/self_contained.rs`, owns three things: a target-directory safety check (regenerate-never-mutate model), a staged-then-atomically-swapped writer for the output tree, and a tar.gz packager. `src/loader.rs` gains a shared internal pull helper so the existing registry pull (used for metadata/validation) and the new materialization pull share one code path, plus a thin function that writes a fragment's `tree/`/`hooks/` payload to disk instead of collecting a path list. `src/generator.rs` gains a `self_contained: bool` parameter (mirroring the existing `ocp: bool`) that swaps `COPY --from=<ref>` / `RUN --mount=...,from=<ref>` for context-relative `COPY fragments/<name>/tree/...` / `RUN --mount=...,source=fragments/<name>/hooks` forms and suppresses every fragment registry reference, including in comments. `src/main.rs` wires the new CLI flag (`conflicts_with = "ocp"`), calls the directory check before any network access, and branches the assembly pipeline's tail between the existing default/OCP output and the new self-contained writer.

**Tech Stack:** Rust, `clap` derive, `tar` + `flate2`, `tempfile`, `anyhow`, `cargo test`

## Global Constraints

- `--self-contained <DIR>` is mutually exclusive with `--ocp`; the CLI itself must refuse both (`clap`'s `conflicts_with`, not a manual check).
- Output tree, exactly:
  ```
  <dir>/
    Containerfile
    manifest.yaml
    fragments/
      <name>/
        tree/
        hooks/            (only when the fragment has hooks)
  <dir>.tar.gz
  ```
- Materialization reuses the loader's existing pull mechanism; no new fetch machinery. Both the metadata/validation pull and the materialization pull go through one shared helper in `src/loader.rs`.
- Regenerate, never mutate: `<dir>` may be absent, empty, or exactly tool-generated (`Containerfile` + `fragments/`, optionally `manifest.yaml`) and nothing else. Anything else is refused. A refused or failed run must never delete or partially overwrite an existing directory.
- Any registry failure during materialization fails the whole run with no partial tree left at `<dir>`. This plan achieves that by staging into a temp directory next to `<dir>` and only renaming it into place after every fragment succeeds.
- `FROM <base>` is the only registry reference anywhere in the self-contained Containerfile, including comments.
- `cargo clippy -- -D clippy::all` must report zero warnings. `cargo fmt --check` must pass. (Exact command per `osfragment-assemble/CLAUDE.md`.)
- Non-goals, hard boundary: no lockfile, no provenance/signature recording, no partial update, no in-place mutation, no archive-suppression flag, no OCP interaction. No task in this plan may build toward any of these; if you find yourself doing so, stop and drop the task.
- Every `LoadedFragment` still has exactly one `FragmentSource` variant (`Registry`), so `let FragmentSource::Registry { ref image_ref } = loaded.source;` remains an irrefutable, valid pattern everywhere it's used.

**Resolved open items (decided in this plan, not reopened by implementers):**

1. **`--pin-digests` does not gate self-contained materialization pulls.** Reading `src/loader.rs::load_registry_fragment` and `src/main.rs::load_all_fragments`: the actual `skopeo copy` for a fragment already always resolves and pulls by digest internally, regardless of `--pin-digests`. That flag only controls whether the *caller* keeps the digest-pinned `FragmentSource`/`resolved_digest` exposed afterward (which drives named-stage emission and digest comments in default mode). Self-contained mode needs the digest-pinned ref to survive past `load_all_fragments` so materialization pulls exactly what was validated, regardless of whether the user asked for digest pinning. So: when `--self-contained` is set, fragment digests are kept (as if `--pin-digests` were set) purely for materialization; the base image's `FROM` line is still pinned only when the user actually passes `--pin-digests`, unchanged from today. Task 10 implements this via `should_keep_fragment_digests(pin_digests, self_contained) -> bool`.
2. **`bind-propagation=rshared` is dropped for the self-contained hook mount; `z` is kept.** Per `process-docs/skills/containerfile-layer-semantics.md`, `bind-propagation` controls how mount events propagate between mount namespaces, which only matters for a live, host-tied mount (MCO's production case mounts `source=/`, a whole image rootfs where nested mounts can appear). A `from=context` source is a static copy of build-context files baked in before the build starts; there is no live mount namespace to propagate events from, so the option is inert there. `z` (SELinux relabel) is unrelated to propagation and still applies to a context-relative bind mount under SELinux enforcement, so it stays. Emitted form: `RUN --mount=type=bind,source=fragments/<name>/hooks,target=/frag-hooks,z \`. Task 8's golden assertions and Task 9's golden file encode this exact string.

---

### Task 1: Target-directory safety check

**Files:**
- Create: `src/self_contained.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `pub fn check_target_dir_safe(dir: &Path) -> Result<()>`, used by Task 2 (CLI wiring) and Task 5 (writer).

- [ ] **Step 1: Register the new module**

In `src/lib.rs`, add the module declaration in alphabetical order:

```rust
pub mod classify;
pub mod fragment;
pub mod generator;
pub mod inspect;
pub mod list;
pub mod loader;
pub mod manifest;
pub mod ocp;
pub mod self_contained;
pub mod validate;
```

- [ ] **Step 2: Write the failing tests**

Create `src/self_contained.rs`:

```rust
//! Self-contained output mode: materializes fragment tree/hooks payload
//! into a local build context next to the generated Containerfile, then
//! packages the result as a sibling tarball. The emitted Containerfile
//! references no registry image except the base.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

/// Entries the tool itself may have written to a self-contained output
/// directory in a prior run. A directory is safe to regenerate only if
/// every entry it contains is one of these.
const TOOL_GENERATED_ENTRIES: &[&str] = &["Containerfile", "manifest.yaml", "fragments"];

/// Refuse to regenerate into a directory that holds anything the tool did
/// not write itself. Absent and empty directories are always safe; a
/// directory containing `Containerfile` and `fragments/` (optionally
/// `manifest.yaml`), and nothing else, is recognized as tool-generated
/// from a prior run and is safe to delete and recreate.
pub fn check_target_dir_safe(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if !dir.is_dir() {
        bail!(
            "--self-contained target {} exists and is not a directory",
            dir.display()
        );
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        entries.push(entry?.file_name().to_string_lossy().to_string());
    }

    if entries.is_empty() {
        return Ok(());
    }

    bail!(
        "--self-contained target {} already exists and was not generated by this tool \
         (expected only Containerfile, manifest.yaml, fragments/); point --self-contained \
         at a new or empty directory",
        dir.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_directory_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("does-not-exist-yet");
        assert!(check_target_dir_safe(&dir).is_ok());
    }

    #[test]
    fn empty_directory_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("empty");
        fs::create_dir(&dir).unwrap();
        assert!(check_target_dir_safe(&dir).is_ok());
    }

    #[test]
    fn tool_generated_directory_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("prior-run");
        fs::create_dir_all(dir.join("fragments/epel")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        fs::write(dir.join("manifest.yaml"), "base: x\n").unwrap();
        assert!(check_target_dir_safe(&dir).is_ok());
    }

    #[test]
    fn foreign_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mine");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("README.md"), "not ours").unwrap();
        let err = check_target_dir_safe(&dir).unwrap_err();
        assert!(err.to_string().contains("was not generated by this tool"));
    }

    #[test]
    fn directory_missing_fragments_is_refused() {
        // Containerfile alone, without fragments/, is not a complete prior
        // run and must not be treated as safe to delete.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("partial");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("Containerfile"), "FROM x\n").unwrap();
        let err = check_target_dir_safe(&dir).unwrap_err();
        assert!(err.to_string().contains("was not generated by this tool"));
    }
}
```

Note: `TOOL_GENERATED_ENTRIES` is declared now for Step 4's implementation to reference; at this point the safety logic is deliberately too strict (any non-empty directory is refused), which is why `tool_generated_directory_is_safe` fails.

- [ ] **Step 3: Run tests to see the expected failure**

Run: `cargo test --lib self_contained:: -- --nocapture`
Expected: `tool_generated_directory_is_safe` FAILS (a non-empty, tool-generated directory is currently refused). The other four tests pass already.

- [ ] **Step 4: Implement the tool-generated recognition**

Replace the body of `check_target_dir_safe` (the `if entries.is_empty()` block through the final `bail!`) with:

```rust
    if entries.is_empty() {
        return Ok(());
    }

    let all_recognized = entries
        .iter()
        .all(|e| TOOL_GENERATED_ENTRIES.contains(&e.as_str()));
    let has_containerfile = entries.iter().any(|e| e == "Containerfile");
    let has_fragments = entries.iter().any(|e| e == "fragments");

    if all_recognized && has_containerfile && has_fragments {
        return Ok(());
    }

    bail!(
        "--self-contained target {} already exists and was not generated by this tool \
         (expected only Containerfile, manifest.yaml, fragments/); point --self-contained \
         at a new or empty directory",
        dir.display()
    );
```

- [ ] **Step 5: Run tests to verify they all pass**

Run: `cargo test --lib self_contained:: -- --nocapture`
Expected: all 5 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/self_contained.rs src/lib.rs
git commit -m "feat: add self-contained target directory safety check

Part of --self-contained mode. A directory is safe to regenerate only
if it is absent, empty, or exactly what a prior tool run produced
(Containerfile + fragments/); anything else is refused rather than
silently overwritten.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 2: CLI flag and early safety gate

**Files:**
- Modify: `src/main.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Consumes: `osfragment_assemble::self_contained::check_target_dir_safe` (Task 1).
- Produces: `Cli.self_contained: Option<PathBuf>`, wired to fail fast via `check_target_dir_safe` before any manifest read or network access.

- [ ] **Step 1: Write the failing CLI tests**

Add to `tests/cli.rs`:

```rust
#[test]
fn self_contained_conflicts_with_ocp() {
    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", "out", "--ocp"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn self_contained_refuses_existing_foreign_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "not ours").unwrap();

    let mut cmd = Command::cargo_bin("osfragment-assemble").unwrap();
    cmd.args(["--self-contained", dir.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("was not generated by this tool"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test cli self_contained -- --nocapture`
Expected: FAIL, `--self-contained` is not a recognized flag yet.

- [ ] **Step 3: Add the CLI flag**

In `src/main.rs`, add to the `Cli` struct, between the `ocp` and `pool` fields:

```rust
    /// Generate a MachineOSConfig YAML for OpenShift on-cluster builds
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "machineosbuild.yaml")]
    ocp: Option<PathBuf>,

    /// Materialize fragment contents into a local build context and
    /// package it as a tarball, so the emitted Containerfile needs no
    /// registry access at build time except for the base image. Mutually
    /// exclusive with --ocp.
    #[arg(long, value_name = "DIR", conflicts_with = "ocp")]
    self_contained: Option<PathBuf>,

    /// MachineConfigPool name for --ocp output (only meaningful with --ocp)
    #[arg(long, default_value = "worker")]
    pool: String,
```

- [ ] **Step 4: Wire the early safety check**

Add the import and the early-exit check at the top of the `None` match arm:

```rust
use osfragment_assemble::self_contained::check_target_dir_safe;
```

(add alongside the existing `use osfragment_assemble::...` imports at the top of the file)

```rust
        None => {
            if let Some(dir) = &cli.self_contained {
                check_target_dir_safe(dir)?;
            }

            // Default: assembly
            let content = std::fs::read_to_string(&cli.manifest)
                .with_context(|| format!("reading manifest {}", cli.manifest.display()))?;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test cli self_contained -- --nocapture`
Expected: both new tests PASS.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS (existing behavior unchanged; `self_contained` defaults to `None`, so the new branch never triggers for existing tests).

- [ ] **Step 7: Commit**

```bash
git add src/main.rs tests/cli.rs
git commit -m "feat: add --self-contained CLI flag with --ocp conflict

--self-contained is mutually exclusive with --ocp (enforced by clap's
conflicts_with) and fails fast on an unsafe target directory before
any manifest read or network access.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 3: Share the registry pull between metadata load and materialization

**Files:**
- Modify: `src/loader.rs`

**Interfaces:**
- Produces: private `fn pull_layer_bytes(image_ref: &str) -> Result<Vec<Vec<u8>>>`, used by `load_registry_fragment` (this task) and `materialize_fragment` (Task 4).
- No behavior change: `load_registry_fragment`'s external behavior (return value, error cases) is identical before and after.

- [ ] **Step 1: Run the baseline test suite**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS. This is the baseline this refactor must not break.

- [ ] **Step 2: Extract the shared pull helper**

In `src/loader.rs`, add this new private function directly above `pub fn load_registry_fragment`:

```rust
/// Pull `image_ref` via skopeo into a temporary OCI layout and return the
/// raw bytes of each layer blob, in manifest order. Shared by every code
/// path that needs a fragment image's layer contents: the full metadata
/// load (`load_registry_fragment`) and self-contained materialization
/// (`materialize_fragment`).
fn pull_layer_bytes(image_ref: &str) -> Result<Vec<Vec<u8>>> {
    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let oci_path = tmp.path().join("oci-layout");

    let status = std::process::Command::new("skopeo")
        .args([
            "copy",
            "--override-os",
            "linux",
            &format!("docker://{}", image_ref),
            &format!("oci:{}", oci_path.display()),
        ])
        .status()
        .context("failed to run skopeo copy")?;

    if !status.success() {
        bail!("skopeo copy failed for {}", image_ref);
    }

    let index_path = oci_path.join("index.json");
    let index_content = std::fs::read_to_string(&index_path)?;
    let index: serde_json::Value = serde_json::from_str(&index_content)?;

    let manifest_desc = index["manifests"]
        .as_array()
        .and_then(|m| m.first())
        .ok_or_else(|| anyhow::anyhow!("no manifests in OCI index"))?;

    let manifest_digest = manifest_desc["digest"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no digest in manifest descriptor"))?;

    let manifest_blob_path = oci_path
        .join("blobs")
        .join(manifest_digest.replace(':', "/"));
    let manifest_content = std::fs::read_to_string(&manifest_blob_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_content)?;

    let layers = manifest["layers"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no layers in manifest"))?;

    layers
        .iter()
        .map(|layer_desc| {
            let layer_digest = layer_desc["digest"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("no digest in layer descriptor"))?;
            let layer_blob_path = oci_path.join("blobs").join(layer_digest.replace(':', "/"));
            std::fs::read(&layer_blob_path)
                .with_context(|| format!("reading layer blob {}", layer_digest))
        })
        .collect()
}
```

- [ ] **Step 3: Rewrite `load_registry_fragment` to use it**

Replace the body of `pub fn load_registry_fragment` (from the `let tmp = tempfile::tempdir()` line through the layer-reading `for layer_desc in layers` loop's opening, i.e. everything up to and including reading `layer_bytes`) so the function becomes:

```rust
pub fn load_registry_fragment(image_ref: &str) -> Result<LoadedFragment> {
    let digest = resolve_digest(image_ref)?;
    let (name, _tag) = split_image_ref(image_ref);
    let image_with_digest = format!("{}@{}", name, digest);

    // Assembly always parses the in-layer fragment.toml for the authoritative
    // Fragment.  The annotation fast path is limited to metadata-only
    // operations (inspect/list via load_registry_fragment_metadata_only)
    // because annotations omit fields like conflicts.
    let layer_bytes_list = pull_layer_bytes(&image_with_digest)?;

    // Scan all layers and aggregate: fragment.toml, tree paths, hooks,
    // and repo file contents may be spread across multiple layers.
    let mut fragment = None;
    let mut all_tree_paths = Vec::new();
    let mut all_hook_paths = Vec::new();
    let mut repo_file_contents = std::collections::HashMap::new();

    for layer_bytes in &layer_bytes_list {
        if fragment.is_none() {
            if let Ok(toml_content) = extract_fragment_toml_from_bytes(layer_bytes) {
                fragment = Some(parse_fragment_toml(&toml_content)?);
            }
        }

        let tree_paths = extract_tree_paths_from_bytes(layer_bytes)?;

        // Collect all executable files in hooks/ directory
        let hook_paths: Vec<PathBuf> = tree_paths
            .iter()
            .filter(|p| p.to_string_lossy().starts_with("fragment/hooks/"))
            .filter_map(|p| p.strip_prefix("fragment/hooks").ok())
            .map(|p| p.to_path_buf())
            .collect();
        all_hook_paths.extend(hook_paths);

        let remapped: Vec<PathBuf> = tree_paths
            .iter()
            .filter_map(|p| p.strip_prefix("fragment").ok())
            .map(|p| p.to_path_buf())
            .collect();
        all_tree_paths.extend(remapped);

        let layer_repo_contents = extract_repo_file_contents_from_bytes(layer_bytes)?;
        repo_file_contents.extend(layer_repo_contents);
    }

    let fragment = fragment.ok_or_else(|| {
        anyhow::anyhow!("no layer containing fragment/fragment.toml found in image")
    })?;
    let relative_paths = all_tree_paths;

    // Sort hooks alphabetically
    all_hook_paths.sort();

    validate_phase_consistency(&fragment, &relative_paths)?;

    Ok(LoadedFragment {
        fragment,
        tree_paths: relative_paths,
        hook_paths: all_hook_paths,
        source: FragmentSource::Registry {
            image_ref: image_with_digest,
        },
        resolved_digest: Some(digest),
        manifest_index: 0, // set by caller
        repo_file_contents,
    })
}
```

This removes the old inline `oci_path`/`index.json`/manifest-blob/layer-loop code (now inside `pull_layer_bytes`) and the old `for layer_desc in layers` loop header (now just `for layer_bytes in &layer_bytes_list`).

- [ ] **Step 4: Run the full test suite to confirm no regression**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS, identical to Step 1's baseline. (No test calls `load_registry_fragment` directly, since it needs network, so this is a build-level plus existing-suite-level confirmation, matching how this function was already validated before this refactor.)

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -- -D clippy::all`
Expected: zero warnings.

- [ ] **Step 6: Commit**

```bash
git add src/loader.rs
git commit -m "refactor: share OCI pull/layer-read between loader paths

Extracts pull_layer_bytes() so load_registry_fragment's skopeo-copy-
then-walk-layers logic can be reused by self-contained materialization
in the next commit, without a second implementation of the same pull.
No behavior change.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 4: Materialize a fragment's tree/hooks payload to disk

**Files:**
- Modify: `src/loader.rs`

**Interfaces:**
- Consumes: `pull_layer_bytes` (Task 3), `validate_tar_entry` (existing).
- Produces: `pub fn materialize_fragment(image_ref: &str, dest_dir: &Path) -> Result<()>`, used by `src/self_contained.rs` (Task 5).

- [ ] **Step 1: Write the failing test for payload extraction**

Add to `src/loader.rs`'s `mod layer_tests` block (after `hooks_collected_regardless_of_extension`):

```rust
    #[test]
    fn payload_extracted_to_disk_matches_source_bytes() {
        let tree_content = b"[epel]\nname=EPEL\nbaseurl=https://example.com/epel/\n";
        let hook_content = b"#!/bin/sh\necho configure\n";
        let tarball = create_test_tarball(&[
            ("fragment/tree/etc/yum.repos.d/epel.repo", tree_content),
            ("fragment/hooks/configure.sh", hook_content),
        ]);

        let workdir = tempfile::tempdir().unwrap();
        extract_fragment_payload_to_disk(&tarball, workdir.path()).unwrap();

        let extracted_tree =
            std::fs::read(workdir.path().join("tree/etc/yum.repos.d/epel.repo")).unwrap();
        let extracted_hook = std::fs::read(workdir.path().join("hooks/configure.sh")).unwrap();
        assert_eq!(extracted_tree, tree_content);
        assert_eq!(extracted_hook, hook_content);
    }

    #[test]
    fn payload_extraction_rejects_traversal_like_other_extractors() {
        let tarball = create_test_tarball(&[
            ("../etc/passwd", b"evil"),
            ("fragment/tree/etc/foo.conf", b"data"),
        ]);
        let workdir = tempfile::tempdir().unwrap();
        let result = extract_fragment_payload_to_disk(&tarball, workdir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("traversal"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib layer_tests::payload -- --nocapture`
Expected: FAIL with "cannot find function `extract_fragment_payload_to_disk`".

- [ ] **Step 3: Implement `extract_fragment_payload_to_disk` and `materialize_fragment`**

Add to `src/loader.rs`, after `extract_repo_file_contents_from_bytes`:

```rust
/// Write a layer's `fragment/tree/` and `fragment/hooks/` payload to disk
/// under `dest_dir/tree` and `dest_dir/hooks`. Shares the same tar-entry
/// security validation as the metadata-only extractors above.
fn extract_fragment_payload_to_disk(compressed: &[u8], dest_dir: &Path) -> Result<()> {
    let decoder = GzDecoder::new(compressed);
    let mut archive = tar::Archive::new(decoder);

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let path = entry.path().context("reading entry path")?.to_path_buf();
        let path_str = path.to_string_lossy().to_string();

        validate_tar_entry(&path_str, entry.header().entry_type())?;

        if !entry.header().entry_type().is_file() {
            continue;
        }

        let dest = if let Ok(rel) = path.strip_prefix("fragment/tree") {
            dest_dir.join("tree").join(rel)
        } else if let Ok(rel) = path.strip_prefix("fragment/hooks") {
            dest_dir.join("hooks").join(rel)
        } else {
            continue;
        };

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        entry
            .unpack(&dest)
            .with_context(|| format!("writing {}", dest.display()))?;
    }
    Ok(())
}

/// Pull a fragment image by reference and materialize its tree/hooks
/// payload to disk under `dest_dir`. Reuses `pull_layer_bytes`, the same
/// skopeo-copy-then-walk-layers path `load_registry_fragment` uses; only
/// the sink differs (files on disk instead of an in-memory path list).
pub fn materialize_fragment(image_ref: &str, dest_dir: &Path) -> Result<()> {
    for layer_bytes in pull_layer_bytes(image_ref)? {
        extract_fragment_payload_to_disk(&layer_bytes, dest_dir)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib layer_tests::payload -- --nocapture`
Expected: both tests PASS.

- [ ] **Step 5: Run the full test suite and clippy**

Run: `cargo test -- --nocapture 2>&1 && cargo clippy -- -D clippy::all`
Expected: all tests PASS, zero clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add src/loader.rs
git commit -m "feat: materialize fragment tree/hooks payload to disk

Adds extract_fragment_payload_to_disk (tar entries -> dest_dir/tree,
dest_dir/hooks, same security validation as the existing metadata
extractors) and materialize_fragment, the pull_layer_bytes-based
wrapper self-contained mode's writer will call per fragment.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 5: Staged, atomic self-contained output writer

**Files:**
- Modify: `src/self_contained.rs`

**Interfaces:**
- Consumes: `crate::loader::materialize_fragment` (Task 4), `crate::loader::LoadedFragment`, `crate::manifest::FragmentSource`, `check_target_dir_safe` (Task 1).
- Produces: `pub fn write_output(dir: &Path, manifest_path: &Path, containerfile: &str, fragments: &[LoadedFragment]) -> Result<()>`, used by `src/main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

Add to `src/self_contained.rs`, above the closing brace of the existing `mod tests` block (i.e. inside it), these imports at the top of the module (outside `mod tests`, alongside the existing `use` lines):

```rust
use crate::loader::LoadedFragment;
use crate::manifest::FragmentSource;
```

Then add these tests inside `mod tests`:

```rust
    fn test_loaded_fragment(name: &str) -> LoadedFragment {
        use crate::fragment::{
            Fragment, FragmentConflicts, FragmentPackages, FragmentPhase, FragmentProvides,
        };
        LoadedFragment {
            fragment: Fragment {
                name: name.to_string(),
                version: "1".into(),
                description: "test".into(),
                vendor: None,
                phase: FragmentPhase::Config,
                provides: FragmentProvides { repos: vec![] },
                packages: FragmentPackages { required: vec![] },
                conflicts: FragmentConflicts { fragments: vec![] },
            },
            tree_paths: vec![],
            hook_paths: vec![],
            source: FragmentSource::Registry {
                image_ref: format!("quay.io/test/{}:1", name),
            },
            resolved_digest: None,
            manifest_index: 0,
            repo_file_contents: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn write_output_stages_then_swaps_atomically() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![test_loaded_fragment("epel"), test_loaded_fragment("cis")];
        write_output_with(&dir, &manifest_path, "FROM example\n", &fragments, |_r, d| {
            fs::create_dir_all(d).map_err(Into::into)
        })
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "FROM example\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("manifest.yaml")).unwrap(),
            "base: example\n"
        );
        assert!(dir.join("fragments/epel").is_dir());
        assert!(dir.join("fragments/cis").is_dir());
    }

    #[test]
    fn write_output_replaces_prior_tool_generated_directory() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(dir.join("fragments/old-frag")).unwrap();
        fs::write(dir.join("Containerfile"), "OLD CONTENT\n").unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        write_output_with(&dir, &manifest_path, "NEW CONTENT\n", &fragments, |_r, d| {
            fs::create_dir_all(d).map_err(Into::into)
        })
        .unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "NEW CONTENT\n"
        );
        assert!(!dir.join("fragments/old-frag").exists());
    }

    #[test]
    fn write_output_refuses_unsafe_target_dir() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), "mine").unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments: Vec<LoadedFragment> = vec![];
        let result = write_output_with(&dir, &manifest_path, "X\n", &fragments, |_r, d| {
            fs::create_dir_all(d).map_err(Into::into)
        });

        assert!(result.is_err());
        assert!(
            dir.join("README.md").exists(),
            "unrelated file must survive a refused run"
        );
    }

    #[test]
    fn write_output_leaves_no_partial_tree_on_materialization_failure() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("ctx");
        fs::create_dir_all(dir.join("fragments/prior-run")).unwrap();
        fs::write(dir.join("Containerfile"), "PRIOR RUN\n").unwrap();

        let manifest_path = workdir.path().join("osfragment-assemble.yaml");
        fs::write(&manifest_path, "base: example\n").unwrap();

        let fragments = vec![
            test_loaded_fragment("good-frag"),
            test_loaded_fragment("bad-frag"),
        ];
        let result = write_output_with(&dir, &manifest_path, "NEW\n", &fragments, |image_ref, dest| {
            if image_ref.contains("bad-frag") {
                bail!("simulated registry failure");
            }
            fs::create_dir_all(dest).map_err(Into::into)
        });

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(dir.join("Containerfile")).unwrap(),
            "PRIOR RUN\n"
        );
        assert!(dir.join("fragments/prior-run").exists());
        assert!(!dir.join("fragments/good-frag").exists());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib self_contained:: -- --nocapture`
Expected: FAIL with "cannot find function `write_output_with`".

- [ ] **Step 3: Implement `write_output_with` and `write_output`**

Add to `src/self_contained.rs`, after `check_target_dir_safe`:

```rust
/// Materialize the self-contained output: fragment tree/hooks payload,
/// the generated Containerfile, and a copy of the input manifest.
///
/// Builds into a staging directory next to `dir` first and swaps it into
/// place only after every fragment materializes successfully, so a
/// registry failure partway through never leaves a partial tree at `dir`.
/// `materialize` is the per-fragment materialization call; production
/// code always passes `crate::loader::materialize_fragment` (see
/// `write_output` below), tests substitute a network-free stub.
fn write_output_with(
    dir: &Path,
    manifest_path: &Path,
    containerfile: &str,
    fragments: &[LoadedFragment],
    materialize: impl Fn(&str, &Path) -> Result<()>,
) -> Result<()> {
    check_target_dir_safe(dir)?;

    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    let staging = tempfile::Builder::new()
        .prefix(".osfragment-assemble-staging-")
        .tempdir_in(parent)
        .context("creating staging directory for self-contained output")?;

    fs::write(staging.path().join("Containerfile"), containerfile)
        .context("writing staged Containerfile")?;
    fs::copy(manifest_path, staging.path().join("manifest.yaml")).with_context(|| {
        format!(
            "copying manifest {} into staged output",
            manifest_path.display()
        )
    })?;

    let staged_fragments = staging.path().join("fragments");
    fs::create_dir_all(&staged_fragments)
        .with_context(|| format!("creating {}", staged_fragments.display()))?;

    for loaded in fragments {
        let FragmentSource::Registry { ref image_ref } = loaded.source;
        let dest = staged_fragments.join(&loaded.fragment.name);
        materialize(image_ref, &dest)
            .with_context(|| format!("materializing fragment '{}'", loaded.fragment.name))?;
    }

    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("removing existing {}", dir.display()))?;
    }
    let staging_path = staging.into_path();
    fs::rename(&staging_path, dir)
        .with_context(|| format!("moving staged output into {}", dir.display()))?;

    Ok(())
}

/// Materialize the self-contained output at `dir` using the real registry
/// pull path.
pub fn write_output(
    dir: &Path,
    manifest_path: &Path,
    containerfile: &str,
    fragments: &[LoadedFragment],
) -> Result<()> {
    write_output_with(
        dir,
        manifest_path,
        containerfile,
        fragments,
        crate::loader::materialize_fragment,
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib self_contained:: -- --nocapture`
Expected: all tests PASS.

- [ ] **Step 5: Run the full test suite and clippy**

Run: `cargo test -- --nocapture 2>&1 && cargo clippy -- -D clippy::all`
Expected: all tests PASS, zero clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add src/self_contained.rs
git commit -m "feat: add staged, atomic self-contained output writer

write_output stages Containerfile, manifest.yaml copy, and per-fragment
materialization into a temp directory next to the target, and only
swaps it into place after every fragment succeeds. A materialization
failure partway through leaves the original directory (if any)
untouched, and the CLI's public entry point always uses the real
registry pull path; tests inject a stub to exercise the atomicity
guarantee without network access.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 6: Tarball packaging

**Files:**
- Modify: `src/self_contained.rs`

**Interfaces:**
- Produces: `pub fn create_archive(dir: &Path) -> Result<PathBuf>`, used by `src/main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

Add to `src/self_contained.rs`'s top-level `use` block:

```rust
use std::path::PathBuf;
```

Add to `mod tests`:

```rust
    #[test]
    fn archive_path_appends_suffix_without_touching_dots() {
        assert_eq!(
            archive_path_for(Path::new("build/context")),
            PathBuf::from("build/context.tar.gz")
        );
        assert_eq!(
            archive_path_for(Path::new("out.v2")),
            PathBuf::from("out.v2.tar.gz")
        );
    }

    #[test]
    fn archive_contents_match_tree_byte_for_byte() {
        let workdir = tempfile::tempdir().unwrap();
        let dir = workdir.path().join("myctx");
        fs::create_dir_all(dir.join("fragments/epel/tree/etc/yum.repos.d")).unwrap();
        fs::create_dir_all(dir.join("fragments/cis/hooks")).unwrap();
        fs::write(dir.join("Containerfile"), "FROM registry.example/base:1\n").unwrap();
        fs::write(dir.join("manifest.yaml"), "apiVersion: bootc.io/v1alpha1\n").unwrap();
        fs::write(
            dir.join("fragments/epel/tree/etc/yum.repos.d/epel.repo"),
            "[epel]\nbaseurl=https://example.com/epel/\n",
        )
        .unwrap();
        fs::write(dir.join("fragments/cis/hooks/configure.sh"), "#!/bin/sh\necho hi\n").unwrap();

        let archive_path = create_archive(&dir).unwrap();
        assert_eq!(
            archive_path,
            PathBuf::from(format!("{}.tar.gz", dir.display()))
        );

        let extract_dir = workdir.path().join("extracted");
        fs::create_dir_all(&extract_dir).unwrap();
        let file = fs::File::open(&archive_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&extract_dir).unwrap();

        let extracted_root = extract_dir.join("myctx");
        for rel in [
            "Containerfile",
            "manifest.yaml",
            "fragments/epel/tree/etc/yum.repos.d/epel.repo",
            "fragments/cis/hooks/configure.sh",
        ] {
            let original = fs::read(dir.join(rel)).unwrap();
            let extracted = fs::read(extracted_root.join(rel)).unwrap();
            assert_eq!(original, extracted, "{} did not round-trip byte for byte", rel);
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib self_contained:: -- --nocapture`
Expected: FAIL with "cannot find function `archive_path_for`" / "cannot find function `create_archive`".

- [ ] **Step 3: Implement `archive_path_for` and `create_archive`**

Add to `src/self_contained.rs`, after `write_output`:

```rust
/// Sibling archive path for a self-contained output directory: `<dir>`
/// with `.tar.gz` appended, e.g. `build/context` -> `build/context.tar.gz`.
/// Appends as a suffix rather than using `Path::with_extension`, so a
/// directory name containing a `.` (e.g. `out.v2`) is not corrupted.
fn archive_path_for(dir: &Path) -> PathBuf {
    let mut s = dir.as_os_str().to_os_string();
    s.push(".tar.gz");
    PathBuf::from(s)
}

/// Package `dir` as a sibling `.tar.gz`, with a single top-level directory
/// named after `dir`'s basename so extraction is predictable regardless of
/// where the archive is unpacked. Returns the archive's path.
pub fn create_archive(dir: &Path) -> Result<PathBuf> {
    let base_name = dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory name", dir.display()))?
        .to_string_lossy()
        .to_string();

    let archive_path = archive_path_for(dir);
    let file = fs::File::create(&archive_path)
        .with_context(|| format!("creating {}", archive_path.display()))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder
        .append_dir_all(&base_name, dir)
        .with_context(|| format!("archiving {} into {}", dir.display(), archive_path.display()))?;
    builder
        .into_inner()
        .context("finishing tar stream")?
        .finish()
        .context("finishing gzip stream")?;

    Ok(archive_path)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib self_contained:: -- --nocapture`
Expected: all tests PASS.

- [ ] **Step 5: Run the full test suite and clippy**

Run: `cargo test -- --nocapture 2>&1 && cargo clippy -- -D clippy::all`
Expected: all tests PASS, zero clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add src/self_contained.rs
git commit -m "feat: package self-contained output as a sibling tar.gz

create_archive wraps dir in a single top-level directory named after
its basename, so extraction is predictable regardless of where the
archive is unpacked. Verified byte-for-byte against the source tree.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 7: Add `self_contained` parameter to the generator

**Files:**
- Modify: `src/generator.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `generate_containerfile(manifest, fragments, base_digest, dedup, ocp: bool, self_contained: bool, capabilities) -> Result<String>` (new 6th parameter, before `capabilities`), used by every existing call site, Task 8 (emission substitutions), and Task 10 (CLI wiring).
- This task changes the signature and two behaviors already inside its scope (skipping fragment FROM stages, suppressing fragment digest comments); Task 8 handles the COPY/mount body substitutions.

- [ ] **Step 1: Change the function signature**

In `src/generator.rs`, change:

```rust
pub fn generate_containerfile(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
    base_digest: Option<&str>,
    _dedup: &DeduplicationResult,
    ocp: bool,
    capabilities: &CapabilitySet,
) -> Result<String> {
```

to:

```rust
/// `self_contained` emits context-relative COPY/mount forms instead of
/// registry references, for `--self-contained` output. Mutually exclusive
/// with `ocp` (enforced by the CLI's `conflicts_with`, not checked here).
pub fn generate_containerfile(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
    base_digest: Option<&str>,
    _dedup: &DeduplicationResult,
    ocp: bool,
    self_contained: bool,
    capabilities: &CapabilitySet,
) -> Result<String> {
```

- [ ] **Step 2: Suppress fragment digest comments in self-contained mode**

Change:

```rust
        // Resolved digests (only when --pin-digests is used)
        let has_digests =
            base_digest.is_some() || fragments.iter().any(|f| f.resolved_digest.is_some());
        if has_digests {
            writeln!(out, "# Resolved digests:")?;
            if let Some(d) = base_digest {
                writeln!(out, "#   base: {}@{}", manifest.base, d)?;
            }
            for loaded in fragments {
                let mf = &manifest.fragments[loaded.manifest_index];
                if let Some(d) = &loaded.resolved_digest {
                    writeln!(out, "#   {}: {}@{}", loaded.fragment.name, mf.image, d)?;
                }
            }
        }
```

to:

```rust
        // Resolved digests. Self-contained mode always resolves fragment
        // digests internally (materialization must pull deterministic
        // content regardless of --pin-digests, see process-docs), but
        // never prints them: no fragment registry reference may appear
        // anywhere in this mode's output, comments included.
        let has_digests = base_digest.is_some()
            || (!self_contained && fragments.iter().any(|f| f.resolved_digest.is_some()));
        if has_digests {
            writeln!(out, "# Resolved digests:")?;
            if let Some(d) = base_digest {
                writeln!(out, "#   base: {}@{}", manifest.base, d)?;
            }
            if !self_contained {
                for loaded in fragments {
                    let mf = &manifest.fragments[loaded.manifest_index];
                    if let Some(d) = &loaded.resolved_digest {
                        writeln!(out, "#   {}: {}@{}", loaded.fragment.name, mf.image, d)?;
                    }
                }
            }
        }
```

- [ ] **Step 3: Skip fragment FROM stages in self-contained mode**

Change:

```rust
    // Fragment FROM stages (only when pinning digests — named stages for readability)
    if use_named_stages {
```

to:

```rust
    // Fragment FROM stages (only when pinning digests, named stages for
    // readability). Self-contained mode never emits these: nothing in the
    // build context comes from a named stage, and no fragment registry
    // reference may appear in the output.
    if use_named_stages && !self_contained {
```

- [ ] **Step 4: Fix every call site (compiler-driven)**

This is a pure arity change: every existing call passes `false` for the new parameter (no existing test exercises self-contained mode; Task 8 and Task 9 add those). Two representative examples:

Multi-line call (most tests use this shape):

```rust
        let output = generate_containerfile(
            &manifest,
            &[epel],
            Some("sha256:base123"),
            &empty_dedup(),
            false,
            &bootc_caps(),
        )
        .unwrap();
```

becomes:

```rust
        let output = generate_containerfile(
            &manifest,
            &[epel],
            Some("sha256:base123"),
            &empty_dedup(),
            false,
            false,
            &bootc_caps(),
        )
        .unwrap();
```

Compact single-line call (a few tests use this shape, e.g. `unpinned_uses_inline_copy_refs`):

```rust
        let output =
            generate_containerfile(&manifest, &[epel], None, &empty_dedup(), false, &bootc_caps())
                .unwrap();
```

becomes:

```rust
        let output = generate_containerfile(
            &manifest,
            &[epel],
            None,
            &empty_dedup(),
            false,
            false,
            &bootc_caps(),
        )
        .unwrap();
```

Apply the same pattern (insert `false,` immediately after the existing `ocp` bool argument, `false` or `true`, and immediately before the `capabilities` argument) to every remaining call site in `src/generator.rs`'s `mod tests`. Run `cargo build --tests` after each batch of edits; it reports "this function takes 7 arguments but N were supplied" with the exact file:line of every remaining mismatched call, so let the compiler drive you to every one: there is no call site it will silently miss.

Then update the two call sites in `src/main.rs`:

```rust
            let containerfile = generate_containerfile(
                &manifest,
                &fragments,
                base_digest.as_deref(),
                &dedup,
                false,
                false,
                &capabilities,
            )?;
```

and

```rust
                let ocp_containerfile = generate_containerfile(
                    &manifest,
                    &fragments,
                    base_digest.as_deref(),
                    &dedup,
                    true,
                    false,
                    &ocp_capabilities,
                )?;
```

- [ ] **Step 5: Run the full test suite until it compiles and passes**

Run: `cargo build --tests 2>&1` repeatedly, fixing each reported call site, until it compiles clean. Then:

Run: `cargo test -- --nocapture 2>&1`
Expected: all existing tests PASS (self_contained=false everywhere reproduces prior output exactly).

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -- -D clippy::all`
Expected: zero warnings.

- [ ] **Step 7: Commit**

```bash
git add src/generator.rs src/main.rs
git commit -m "feat: add self_contained parameter to generate_containerfile

Mechanical arity change (every existing call passes false) plus two
behaviors already scoped to this parameter: fragment FROM stages and
fragment digest comments are suppressed in self-contained mode, since
no fragment registry reference may appear anywhere in that mode's
output. COPY/mount body substitutions land in the next commit.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 8: Context-relative COPY and hook mount emission

**Files:**
- Modify: `src/generator.rs`

**Interfaces:**
- Consumes: `self_contained` (Task 7), `copy_from_source` (existing, still used for `!self_contained`).
- Produces: self-contained-mode emission for repo COPY, config COPY, and hook `RUN --mount`, per the two resolved open items in Global Constraints.

- [ ] **Step 1: Write the failing test**

Add to `src/generator.rs`'s `mod tests`:

```rust
    #[test]
    fn self_contained_uses_context_relative_copy_and_mount() {
        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let (mut cis, mf_cis) = make_config_fragment("cis", "bbb222");
        cis.manifest_index = 1;
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            base_type: None,
            fragments: vec![mf_epel, mf_cis],
        };
        let output = generate_containerfile(
            &manifest,
            &[epel, cis],
            None,
            &empty_dedup(),
            false,
            true,
            &bootc_caps(),
        )
        .unwrap();

        assert!(output.contains("COPY fragments/epel/tree/etc/yum.repos.d/ /etc/yum.repos.d/"));
        assert!(output.contains("COPY fragments/epel/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/"));
        assert!(output.contains("COPY fragments/cis/tree/ /"));
        assert!(output.contains(
            "RUN --mount=type=bind,source=fragments/cis/hooks,target=/frag-hooks,z \\"
        ));
        assert!(output.contains("/frag-hooks/configure.sh"));

        // The mode's defining invariant: no fragment registry reference
        // anywhere, including comments, and no leftover default-mode forms.
        assert!(!output.contains("bind-propagation"));
        assert!(!output.contains("--from="));
        assert!(!output.contains("AS frag-"));
        assert!(!output.contains("quay.io/test/epel"));
        assert!(!output.contains("quay.io/test/cis"));
        let from_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("FROM")).collect();
        assert_eq!(
            from_lines,
            vec!["FROM registry.redhat.io/rhel10/rhel-bootc:10.0"]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test self_contained_uses_context_relative_copy_and_mount -- --nocapture`
Expected: FAIL, current self-contained output still uses `COPY --from=` and `bind-propagation=rshared`.

- [ ] **Step 3: Replace repo file COPY emission**

Change:

```rust
        for loaded in fragments {
            let has_repo = loaded.tree_paths.iter().any(|p| is_repo_path(p));
            if !has_repo {
                continue;
            }
            let source = copy_from_source(loaded, use_named_stages);
            if loaded
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("yum.repos.d"))
            {
                writeln!(
                    out,
                    "COPY --from={} /fragment/tree/etc/yum.repos.d/ /etc/yum.repos.d/",
                    source
                )?;
            }
            if loaded
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("rpm-gpg"))
            {
                writeln!(
                    out,
                    "COPY --from={} /fragment/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/",
                    source
                )?;
            }
        }
```

to:

```rust
        for loaded in fragments {
            let has_repo = loaded.tree_paths.iter().any(|p| is_repo_path(p));
            if !has_repo {
                continue;
            }
            if loaded
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("yum.repos.d"))
            {
                if self_contained {
                    writeln!(
                        out,
                        "COPY fragments/{}/tree/etc/yum.repos.d/ /etc/yum.repos.d/",
                        loaded.fragment.name
                    )?;
                } else {
                    let source = copy_from_source(loaded, use_named_stages);
                    writeln!(
                        out,
                        "COPY --from={} /fragment/tree/etc/yum.repos.d/ /etc/yum.repos.d/",
                        source
                    )?;
                }
            }
            if loaded
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().contains("rpm-gpg"))
            {
                if self_contained {
                    writeln!(
                        out,
                        "COPY fragments/{}/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/",
                        loaded.fragment.name
                    )?;
                } else {
                    let source = copy_from_source(loaded, use_named_stages);
                    writeln!(
                        out,
                        "COPY --from={} /fragment/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/",
                        source
                    )?;
                }
            }
        }
```

- [ ] **Step 4: Replace config file COPY emission**

Change:

```rust
        for loaded in fragments {
            let has_non_repo = loaded
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().starts_with("tree/") && !is_repo_path(p));
            if !has_non_repo {
                continue;
            }
            let source = copy_from_source(loaded, use_named_stages);
            writeln!(out, "COPY --from={} /fragment/tree/ /", source)?;
        }
```

to:

```rust
        for loaded in fragments {
            let has_non_repo = loaded
                .tree_paths
                .iter()
                .any(|p| p.to_string_lossy().starts_with("tree/") && !is_repo_path(p));
            if !has_non_repo {
                continue;
            }
            if self_contained {
                writeln!(out, "COPY fragments/{}/tree/ /", loaded.fragment.name)?;
            } else {
                let source = copy_from_source(loaded, use_named_stages);
                writeln!(out, "COPY --from={} /fragment/tree/ /", source)?;
            }
        }
```

- [ ] **Step 5: Replace hook mount emission**

Change:

```rust
        for loaded in &hook_fragments {
            let source = copy_from_source(loaded, use_named_stages);

            // Build chained hook invocations
            let hook_cmds: Vec<String> = loaded
                .hook_paths
                .iter()
                .map(|h| format!("/frag-hooks/{}", h.display()))
                .collect();
            let chained = hook_cmds.join(" && ");

            writeln!(
                out,
                "RUN --mount=type=bind,from={},source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z \\",
                source
            )?;
            writeln!(out, "    {}", chained)?;
        }
```

to:

```rust
        for loaded in &hook_fragments {
            // Build chained hook invocations
            let hook_cmds: Vec<String> = loaded
                .hook_paths
                .iter()
                .map(|h| format!("/frag-hooks/{}", h.display()))
                .collect();
            let chained = hook_cmds.join(" && ");

            if self_contained {
                writeln!(
                    out,
                    "RUN --mount=type=bind,source=fragments/{}/hooks,target=/frag-hooks,z \\",
                    loaded.fragment.name
                )?;
            } else {
                let source = copy_from_source(loaded, use_named_stages);
                writeln!(
                    out,
                    "RUN --mount=type=bind,from={},source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z \\",
                    source
                )?;
            }
            writeln!(out, "    {}", chained)?;
        }
```

- [ ] **Step 6: Run the new test and the full suite**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS, including `self_contained_uses_context_relative_copy_and_mount`.

- [ ] **Step 7: Run clippy**

Run: `cargo clippy -- -D clippy::all`
Expected: zero warnings.

- [ ] **Step 8: Commit**

```bash
git add src/generator.rs
git commit -m "feat: emit context-relative COPY/mount in self-contained mode

Repo and config COPY drop --from= entirely (fragments/<name>/tree/...
resolves against the build context). The hook bind mount drops from=
and bind-propagation=rshared (meaningless for a static context-relative
source per containerfile-layer-semantics.md) but keeps z. Verifies the
mode's defining invariant: no fragment registry reference anywhere,
comments included.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 9: Golden-file test for the full self-contained Containerfile

**Files:**
- Modify: `src/generator.rs`

**Interfaces:**
- Consumes: `generate_containerfile` with `self_contained: true` (Tasks 7-8), `make_repos_fragment`/`make_config_fragment`/`empty_dedup`/`bootc_caps` (existing test helpers).

- [ ] **Step 1: Write the golden-file test**

Add to `src/generator.rs`'s `mod tests`:

```rust
    #[test]
    fn self_contained_output_matches_golden_containerfile() {
        const EXPECTED: &str = r#"# Generated by osfragment-assemble v0.1.0
# Manifest: osfragment-assemble.yaml
# Fragments: epel (repos), cis (config)
# Override summary: no file path collisions detected

# --- Base ---
FROM registry.redhat.io/rhel10/rhel-bootc:10.0

# --- Repo files ---
COPY fragments/epel/tree/etc/yum.repos.d/ /etc/yum.repos.d/
COPY fragments/epel/tree/etc/pki/rpm-gpg/ /etc/pki/rpm-gpg/

# --- Packages ---
RUN dnf install -y \
        htop \
    && dnf clean all \
    && rm -rf /var/cache/dnf /var/log/dnf* /var/log/hawkey.log /var/lib/dnf/history.sqlite*

# --- Config files ---
COPY fragments/cis/tree/ /

# --- Hooks ---
RUN --mount=type=bind,source=fragments/cis/hooks,target=/frag-hooks,z \
    /frag-hooks/configure.sh

# Apply systemd presets from fragments
RUN systemctl preset-all --preset-mode=enable-only 2>/dev/null || true

# --- Phase: validation (90) ---
RUN bootc container lint
"#;

        let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        let (mut cis, mf_cis) = make_config_fragment("cis", "bbb222");
        cis.manifest_index = 1;
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            base_type: None,
            fragments: vec![mf_epel, mf_cis],
        };
        let output = generate_containerfile(
            &manifest,
            &[epel, cis],
            None,
            &empty_dedup(),
            false,
            true,
            &bootc_caps(),
        )
        .unwrap();

        assert_eq!(output, EXPECTED);
    }
```

- [ ] **Step 2: Run the test**

Run: `cargo test self_contained_output_matches_golden_containerfile -- --nocapture`
Expected: PASS. If it fails, the assertion failure prints both strings; diff them and fix Task 8's emission code (not this test) unless the golden string itself has a transcription error against the fixtures' actual field values (`aaa111`/`bbb222` digests aren't in the output since self-contained mode suppresses them, `htop` from `mf_epel.packages`, `configure.sh` from `make_config_fragment`'s hook).

- [ ] **Step 3: Run the full suite and clippy**

Run: `cargo test -- --nocapture 2>&1 && cargo clippy -- -D clippy::all`
Expected: all tests PASS, zero clippy warnings.

- [ ] **Step 4: Commit**

```bash
git add src/generator.rs
git commit -m "test: add golden-file test for self-contained Containerfile

Exact-match test against a mixed repos+config+hooks fixture, covering
header suppression, context-relative repo/config COPY, and the hook
mount's dropped bind-propagation in one assertion.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 10: Wire the CLI end to end

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `write_output`, `create_archive` (Tasks 5-6), `generate_containerfile` with `self_contained` (Tasks 7-8).
- Produces: `fn should_keep_fragment_digests(pin_digests: bool, self_contained: Option<&Path>) -> bool` (Resolved Open Item 1), full `--self-contained` assembly branch.

- [ ] **Step 1: Write the failing test for the digest-keeping decision**

Add near the bottom of `src/main.rs` (after the `fn load_all_fragments` definition):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_fragment_digests_cases() {
        assert!(!should_keep_fragment_digests(false, None));
        assert!(should_keep_fragment_digests(
            false,
            Some(Path::new("out"))
        ));
        assert!(should_keep_fragment_digests(true, None));
        assert!(should_keep_fragment_digests(true, Some(Path::new("out"))));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin osfragment-assemble keep_fragment_digests -- --nocapture`
Expected: FAIL with "cannot find function `should_keep_fragment_digests`".

- [ ] **Step 3: Update imports**

Change:

```rust
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use osfragment_assemble::classify::{classify_base, capabilities_for_base_type};
use osfragment_assemble::generator::generate_containerfile;
use osfragment_assemble::inspect::run_inspect;
use osfragment_assemble::list::run_list;
use osfragment_assemble::loader::{
    load_registry_fragment, load_registry_fragment_metadata_only, resolve_digest,
};
use osfragment_assemble::manifest::{parse_manifest, BaseType};
use osfragment_assemble::ocp::generate_machine_os_config;
use osfragment_assemble::self_contained::check_target_dir_safe;
use osfragment_assemble::validate::validate_composition;
```

to:

```rust
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use osfragment_assemble::classify::{classify_base, capabilities_for_base_type};
use osfragment_assemble::generator::generate_containerfile;
use osfragment_assemble::inspect::run_inspect;
use osfragment_assemble::list::run_list;
use osfragment_assemble::loader::{
    load_registry_fragment, load_registry_fragment_metadata_only, resolve_digest,
};
use osfragment_assemble::manifest::{parse_manifest, BaseType};
use osfragment_assemble::ocp::generate_machine_os_config;
use osfragment_assemble::self_contained::{check_target_dir_safe, create_archive, write_output};
use osfragment_assemble::validate::validate_composition;
```

- [ ] **Step 4: Add `should_keep_fragment_digests`**

Add after the `fn load_all_fragments` function definition (before the new `#[cfg(test)]` block from Step 1):

```rust
/// Whether fragment digests (and the digest-pinned `FragmentSource`) should
/// survive `load_all_fragments` for use downstream.
///
/// `--pin-digests` keeps them for default mode's named-stage emission and
/// digest comments, as before. `--self-contained` also needs them kept,
/// independently of `--pin-digests`: materialization must pull exactly the
/// digest that was validated, even though the emitted Containerfile never
/// exposes that digest (see generator.rs's self-contained suppression).
fn should_keep_fragment_digests(pin_digests: bool, self_contained: Option<&Path>) -> bool {
    pin_digests || self_contained.is_some()
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --bin osfragment-assemble keep_fragment_digests -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Rewrite the `None` match arm to branch on `--self-contained`**

Replace the entire `None => { ... }` arm (from `if let Some(dir) = &cli.self_contained { check_target_dir_safe(dir)?; }` through its closing brace) with:

```rust
        None => {
            if let Some(dir) = &cli.self_contained {
                check_target_dir_safe(dir)?;
            }

            // Default: assembly
            let content = std::fs::read_to_string(&cli.manifest)
                .with_context(|| format!("reading manifest {}", cli.manifest.display()))?;
            let manifest = parse_manifest(&content)?;

            let base_digest = if cli.pin_digests {
                eprintln!("Resolving base image digest...");
                Some(resolve_digest(&manifest.base)?)
            } else {
                None
            };

            let keep_digests =
                should_keep_fragment_digests(cli.pin_digests, cli.self_contained.as_deref());
            let fragments = load_all_fragments(&manifest, keep_digests)?;

            eprintln!("Validating composition...");
            let dedup = validate_composition(&manifest, &fragments)?;

            // Classify the base image
            eprintln!("Classifying base image...");
            let capabilities = classify_base(&manifest.base, manifest.base_type.as_ref());

            if let Some(dir) = &cli.self_contained {
                let containerfile = generate_containerfile(
                    &manifest,
                    &fragments,
                    base_digest.as_deref(),
                    &dedup,
                    false,
                    true,
                    &capabilities,
                )?;

                write_output(dir, &cli.manifest, &containerfile, &fragments)?;
                let archive_path = create_archive(dir)?;

                eprintln!(
                    "Self-contained context written to {} ({} fragments)",
                    dir.display(),
                    fragments.len()
                );
                eprintln!("Archive written to {}", archive_path.display());
            } else {
                let containerfile = generate_containerfile(
                    &manifest,
                    &fragments,
                    base_digest.as_deref(),
                    &dedup,
                    false,
                    false,
                    &capabilities,
                )?;

                std::fs::write(&cli.output, &containerfile)
                    .with_context(|| format!("writing {}", cli.output.display()))?;

                eprintln!(
                    "Containerfile written to {} ({} fragments)",
                    cli.output.display(),
                    fragments.len()
                );

                // OCP MachineOSConfig generation — always uses bootc capabilities
                if let Some(ocp_path) = &cli.ocp {
                    let ocp_capabilities = capabilities_for_base_type(BaseType::Bootc);
                    let ocp_containerfile = generate_containerfile(
                        &manifest,
                        &fragments,
                        base_digest.as_deref(),
                        &dedup,
                        true,
                        false,
                        &ocp_capabilities,
                    )?;
                    let mosc = generate_machine_os_config(&ocp_containerfile, &cli.pool)?;
                    std::fs::write(ocp_path, &mosc)
                        .with_context(|| format!("writing {}", ocp_path.display()))?;
                    eprintln!("MachineOSConfig written to {}", ocp_path.display());
                }
            }
        }
```

- [ ] **Step 7: Update `load_all_fragments`'s parameter name**

Change:

```rust
fn load_all_fragments(
    manifest: &osfragment_assemble::manifest::Manifest,
    pin_digests: bool,
) -> Result<Vec<osfragment_assemble::loader::LoadedFragment>> {
    let mut fragments = Vec::new();
    let total = manifest.fragments.len();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = mf.resolve_source()?;
        let osfragment_assemble::manifest::FragmentSource::Registry { ref image_ref } = source;
        eprintln!("Loading fragment {}/{}: {}...", idx + 1, total, image_ref);
        let mut loaded = load_registry_fragment(image_ref)?;
        if !pin_digests {
            // Use the manifest's declared image ref, not the digest-pinned ref
            loaded.source = osfragment_assemble::manifest::FragmentSource::Registry {
                image_ref: image_ref.clone(),
            };
            loaded.resolved_digest = None;
        }
```

to:

```rust
/// `keep_digests`: whether to leave each fragment's digest-pinned
/// `FragmentSource`/`resolved_digest` in place. See
/// `should_keep_fragment_digests` for why this isn't simply `pin_digests`.
fn load_all_fragments(
    manifest: &osfragment_assemble::manifest::Manifest,
    keep_digests: bool,
) -> Result<Vec<osfragment_assemble::loader::LoadedFragment>> {
    let mut fragments = Vec::new();
    let total = manifest.fragments.len();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = mf.resolve_source()?;
        let osfragment_assemble::manifest::FragmentSource::Registry { ref image_ref } = source;
        eprintln!("Loading fragment {}/{}: {}...", idx + 1, total, image_ref);
        let mut loaded = load_registry_fragment(image_ref)?;
        if !keep_digests {
            // Use the manifest's declared image ref, not the digest-pinned ref
            loaded.source = osfragment_assemble::manifest::FragmentSource::Registry {
                image_ref: image_ref.clone(),
            };
            loaded.resolved_digest = None;
        }
```

(the rest of `load_all_fragments`'s body, from `eprintln!("  {} ({})", ...)` to the closing brace, is unchanged)

- [ ] **Step 8: Run the full test suite and clippy**

Run: `cargo test -- --nocapture 2>&1 && cargo clippy -- -D clippy::all`
Expected: all tests PASS (including Task 2's CLI tests, which now exercise the fully wired path), zero clippy warnings.

- [ ] **Step 9: Run `cargo fmt --check`**

Run: `cargo fmt --check`
Expected: no diff.

- [ ] **Step 10: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire --self-contained through the assembly pipeline

should_keep_fragment_digests centralizes Resolved Open Item 1: fragment
digests are kept for materialization whenever --self-contained is set,
independently of --pin-digests. The None arm now branches between the
existing default/OCP output and write_output + create_archive for
self-contained output.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 11: Final verification

**Files:**
- None (verification only).

- [ ] **Step 1: Run clippy with the project's exact bar**

Run: `cargo clippy -- -D clippy::all`
Expected: zero warnings.

- [ ] **Step 2: Run `cargo fmt --check`**

Run: `cargo fmt --check`
Expected: no diff.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS, including every test added in Tasks 1-10.

- [ ] **Step 4: Confirm no non-goal crept in**

Grep for anything that would indicate scope creep: `grep -rn "lockfile\|provenance\|signature" src/self_contained.rs src/loader.rs src/generator.rs src/main.rs`
Expected: no matches (or only matches in comments explicitly citing the non-goal, e.g. Task 1's doc comments referencing "Resolved Open Item"). If real lockfile/provenance/partial-update code is found, remove it before proceeding; it does not belong in this feature per the spec's Non-goals.

- [ ] **Step 5: If any failures, fix and commit individually**

Fix any issues found with focused commits, each describing the specific fix.

---

## Self-Review

**Spec coverage** (spec section -> task):

- Surface (`--self-contained <dir>`, mutually exclusive with `--ocp`, upstream unchanged) -> Tasks 2, 10.
- Output tree shape (`Containerfile`, `manifest.yaml`, `fragments/<name>/{tree,hooks}`, sibling `.tar.gz`) -> Tasks 5, 6, 10.
- Emission changes (context-relative COPY, hook mount drops `from=`) -> Task 8.
- Update model (regenerate, never mutate; delete-and-recreate in full) -> Tasks 1, 5.
- Errors (`--self-contained` + `--ocp`; unsafe target dir; registry failure leaves no partial tree) -> Tasks 1, 2, 5.
- Acceptance: golden-file test -> Task 9. Integration test (materialize + archive byte-for-byte) -> Tasks 4, 6. No-registry-reference test -> Task 8. Foreign-dir-refused test -> Tasks 1, 2. `--self-contained` + `--ocp` errors test -> Task 2.
- Non-goals (no lockfile, no provenance, no partial update, no archive-suppression flag, no OCP interaction) -> verified absent in Task 11, Step 4.
- Open item 1 (`--pin-digests` interaction) -> resolved in Global Constraints, implemented in Task 10.
- Open item 2 (`bind-propagation` meaningfulness) -> resolved in Global Constraints, implemented in Task 8, asserted in Tasks 8-9.

**Placeholder scan:** no "TBD"/"TODO"/"handle edge cases" markers; every step has runnable code or an exact command.

**Type consistency:** `generate_containerfile`'s new parameter is `self_contained: bool` in every task that touches it (7, 8, 9, 10). `write_output`/`write_output_with`/`materialize_fragment`/`extract_fragment_payload_to_disk`/`check_target_dir_safe`/`create_archive`/`archive_path_for`/`pull_layer_bytes`/`should_keep_fragment_digests` are named and typed identically everywhere they are consumed across tasks.
