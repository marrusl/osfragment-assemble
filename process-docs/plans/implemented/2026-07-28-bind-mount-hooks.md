# Bind Mount Hook Emission Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace COPY-based hook emission with `RUN --mount=type=bind` so hooks never land in any image layer, eliminating the current defect where `rm -rf` only writes a whiteout while hook bytes persist in the earlier layer.

**Architecture:** The hook emission section of `generate_containerfile()` in `generator.rs` (lines 279-311) is replaced. Instead of `COPY --from=<source> /fragment/hooks/ /tmp/...` followed by `RUN ... && rm -rf`, a single `RUN --mount=type=bind,from=<source>,source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z` instruction is emitted per fragment. The `from=` value uses the same `copy_from_source()` resolution already used by tree COPYs, ensuring digest-pinned and unpinned modes stay consistent. No COPY fallback exists.

**Tech Stack:** Rust, `std::fmt::Write`, Cargo test

## Global Constraints

- No COPY fallback mode. Bind mount is the only hook emission path for all output (standalone and OCP).
- Mount options `bind-propagation=rshared,z` are mandatory on every emitted mount, matching MCO's on-cluster-build template.
- `from=` resolution must use `copy_from_source()`, the same function tree COPYs use. Under `--pin-digests`, hooks resolve to the named stage (digest-pinned); without, they resolve to the inline image ref.
- One `RUN --mount` instruction per fragment, not per hook. Hooks chain with `&&`.
- `tree/` content continues to use COPY. Only hooks change.
- `hooks/` must never appear in a COPY instruction in any output path.
- The mount target is `/frag-hooks` (no `/tmp/` prefix, no fragment name disambiguation needed since each mount is scoped to its own instruction).

---

### Task 1: Add negative test — hooks must never appear in COPY

**Files:**
- Modify: `src/generator.rs:1037-1062` (add new test after `ocp_hooks_without_section_comments`)

**Interfaces:**
- Consumes: existing `make_unpinned_config_fragment`, `make_config_fragment`, `generate_containerfile`, `empty_dedup`
- Produces: test `hooks_never_in_copy_instruction` — will fail against current code, pass after Task 2

- [ ] **Step 1: Write the failing test**

Add this test to the `mod tests` block in `src/generator.rs`, after the `ocp_hooks_without_section_comments` test:

```rust
#[test]
fn hooks_never_in_copy_instruction() {
    // Unpinned standalone
    let (mut cis, mf_cis) = make_unpinned_config_fragment("cis");
    cis.manifest_index = 0;
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_cis.clone()],
    };
    let standalone = generate_containerfile(
        &manifest,
        &[cis.clone()],
        None,
        &empty_dedup(),
        false,
    )
    .unwrap();
    for line in standalone.lines() {
        if line.starts_with("COPY") {
            assert!(
                !line.contains("hooks"),
                "hooks/ must not appear in COPY instruction (standalone):\n{}",
                line
            );
        }
    }

    // Unpinned OCP
    let ocp_output = generate_containerfile(
        &manifest,
        &[cis],
        None,
        &empty_dedup(),
        true,
    )
    .unwrap();
    for line in ocp_output.lines() {
        if line.starts_with("COPY") {
            assert!(
                !line.contains("hooks"),
                "hooks/ must not appear in COPY instruction (OCP):\n{}",
                line
            );
        }
    }

    // Pinned standalone
    let (mut pinned_cis, mf_pinned) = make_config_fragment("cis", "bbb222");
    pinned_cis.manifest_index = 0;
    let pinned_manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_pinned],
    };
    let pinned_output = generate_containerfile(
        &pinned_manifest,
        &[pinned_cis],
        Some("sha256:base123"),
        &empty_dedup(),
        false,
    )
    .unwrap();
    for line in pinned_output.lines() {
        if line.starts_with("COPY") {
            assert!(
                !line.contains("hooks"),
                "hooks/ must not appear in COPY instruction (pinned):\n{}",
                line
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test hooks_never_in_copy_instruction -- --nocapture`
Expected: FAIL — current code emits `COPY --from=... /fragment/hooks/`

- [ ] **Step 3: Commit the failing test**

```bash
git add src/generator.rs
git commit -m "test: add negative test — hooks must never appear in COPY

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 2: Replace COPY+RUN hook emission with RUN --mount=type=bind

**Files:**
- Modify: `src/generator.rs:279-311` (the `// Hooks` section)

**Interfaces:**
- Consumes: `copy_from_source()` for `from=` resolution, `LoadedFragment.hook_paths`
- Produces: `RUN --mount=type=bind,...` instruction instead of `COPY` + `RUN` + `rm -rf`

- [ ] **Step 1: Replace the hook emission block**

Replace lines 279-311 of `src/generator.rs` (the entire `// Hooks (must run after packages are installed)` section) with:

```rust
    // Hooks (must run after packages are installed)
    let hook_fragments: Vec<_> = fragments
        .iter()
        .filter(|f| !f.hook_paths.is_empty())
        .collect();
    if !hook_fragments.is_empty() {
        if !ocp {
            writeln!(out, "# --- Hooks ---")?;
        }
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
        if !ocp {
            writeln!(out)?;
        }
    }
```

- [ ] **Step 2: Run the negative test to verify it passes**

Run: `cargo test hooks_never_in_copy_instruction -- --nocapture`
Expected: PASS — no COPY instruction references hooks

- [ ] **Step 3: Run the full test suite to see what breaks**

Run: `cargo test -- --nocapture 2>&1`
Expected: several existing tests fail because they assert the old COPY-based hook pattern. Note which tests fail for Task 3.

- [ ] **Step 4: Commit the implementation**

```bash
git add src/generator.rs
git commit -m "feat: emit hooks via RUN --mount=type=bind instead of COPY

Hooks are now executed via bind mount, eliminating the defect where
hook bytes persisted in earlier image layers despite the rm -rf cleanup.
Mount options mirror MCO's on-cluster-build template.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 3: Update existing tests to expect bind mount output

**Files:**
- Modify: `src/generator.rs` (test module, multiple tests)

**Interfaces:**
- Consumes: output of Task 2 (new hook emission format)
- Produces: all existing hook-related tests pass with the new format

The following tests assert the old COPY-based hook pattern and must be updated:

- [ ] **Step 1: Update `unpinned_hooks_use_inline_copy_refs` (line 759)**

Replace the test body:

```rust
#[test]
fn unpinned_hooks_use_bind_mount() {
    let (mut cis, mf_cis) = make_unpinned_config_fragment("cis");
    cis.manifest_index = 0;
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_cis],
    };
    let output =
        generate_containerfile(&manifest, &[cis], None, &empty_dedup(), false).unwrap();
    assert!(
        output.contains("RUN --mount=type=bind,from=quay.io/test/cis:2.1,source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z"),
        "expected bind mount with inline image ref:\n{}",
        output
    );
    assert!(
        output.contains("/frag-hooks/configure.sh"),
        "expected hook execution via mount target:\n{}",
        output
    );
    // No COPY of hooks, no rm -rf cleanup
    assert!(
        !output.contains("COPY --from=quay.io/test/cis:2.1 /fragment/hooks/"),
        "COPY of hooks must not appear:\n{}",
        output
    );
    assert!(
        !output.contains("rm -rf"),
        "rm -rf cleanup must not appear:\n{}",
        output
    );
}
```

- [ ] **Step 2: Update `multiple_hooks_run_in_alphabetical_order` (line 889)**

Replace the test body:

```rust
#[test]
fn multiple_hooks_run_in_alphabetical_order() {
    let mut loaded = LoadedFragment {
        fragment: Fragment {
            name: "multi-hook".to_string(),
            version: "1.0".into(),
            description: "test".into(),
            vendor: None,
            phase: FragmentPhase::Config,
            provides: FragmentProvides { repos: vec![] },
            packages: FragmentPackages { available: vec![] },
            conflicts: FragmentConflicts { fragments: vec![] },
        },
        tree_paths: vec![],
        hook_paths: vec![
            PathBuf::from("03-final.sh"),
            PathBuf::from("01-first.bash"),
            PathBuf::from("02-second.sh"),
        ],
        source: FragmentSource::Registry {
            image_ref: "quay.io/test/multi-hook:1.0".into(),
        },
        resolved_digest: None,
        manifest_index: 0,
        repo_file_contents: std::collections::HashMap::new(),
    };
    loaded.hook_paths.sort(); // Simulate what loader does
    let manifest_frag = ManifestFragment {
        image: "quay.io/test/multi-hook:1.0".into(),
        packages: vec![],
        mirror: None,
    };
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![manifest_frag],
    };
    let output =
        generate_containerfile(&manifest, &[loaded], None, &empty_dedup(), false).unwrap();

    // Verify all three hooks are present via the mount target path
    assert!(output.contains("/frag-hooks/01-first.bash"));
    assert!(output.contains("/frag-hooks/02-second.sh"));
    assert!(output.contains("/frag-hooks/03-final.sh"));

    // Verify alphabetical ordering by checking positions
    let first_pos = output
        .find("/frag-hooks/01-first.bash")
        .unwrap();
    let second_pos = output
        .find("/frag-hooks/02-second.sh")
        .unwrap();
    let third_pos = output
        .find("/frag-hooks/03-final.sh")
        .unwrap();
    assert!(first_pos < second_pos);
    assert!(second_pos < third_pos);

    // Verify single RUN --mount instruction (not per-hook)
    let mount_count = output.matches("RUN --mount=type=bind").count();
    assert_eq!(
        mount_count, 1,
        "expected exactly one RUN --mount instruction:\n{}",
        output
    );
}
```

- [ ] **Step 3: Update `mixed_repos_and_config_full_content_ordering` (line 973)**

The test asserts ordering `config_copy < hook_copy` where `hook_copy` searches for `COPY --from=frag-cis /fragment/hooks/`. Replace the hook assertion:

```rust
#[test]
fn mixed_repos_and_config_full_content_ordering() {
    let (epel, mf_epel) = make_repos_fragment("epel", "aaa111");
    let (mut cis, mf_cis) = make_config_fragment("cis", "bbb222");
    cis.manifest_index = 1;
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_epel, mf_cis],
    };
    let output = generate_containerfile(
        &manifest,
        &[epel, cis],
        Some("sha256:base123"),
        &empty_dedup(),
        false,
    )
    .unwrap();
    // Verify full content ordering: repo COPY -> dnf install -> config COPY -> hook RUN --mount
    let repo_copy = output
        .find("COPY --from=frag-epel /fragment/tree/etc/yum.repos.d/")
        .expect("missing repo COPY");
    let dnf_install = output.find("dnf install -y").expect("missing dnf install");
    let config_copy = output
        .find("COPY --from=frag-cis /fragment/tree/ /")
        .expect("missing config COPY");
    let hook_mount = output
        .find("RUN --mount=type=bind,from=frag-cis")
        .expect("missing hook RUN --mount");
    assert!(repo_copy < dnf_install);
    assert!(dnf_install < config_copy);
    assert!(config_copy < hook_mount);
}
```

- [ ] **Step 4: Update `ocp_hooks_without_section_comments` (line 1037)**

Replace the hook assertions:

```rust
#[test]
fn ocp_hooks_use_bind_mount() {
    let (mut cis, mf_cis) = make_unpinned_config_fragment("cis");
    cis.manifest_index = 0;
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_cis],
    };
    let output = generate_containerfile(&manifest, &[cis], None, &empty_dedup(), true).unwrap();
    // Hooks should use bind mount in OCP output
    assert!(
        output.contains("RUN --mount=type=bind"),
        "expected RUN --mount in OCP output:\n{}",
        output
    );
    assert!(
        output.contains("/frag-hooks/configure.sh"),
        "expected hook execution in OCP output:\n{}",
        output
    );
    // No section comment headers in OCP mode
    assert!(
        !output.contains("# ---"),
        "OCP output should not contain section comments:\n{}",
        output
    );
}
```

- [ ] **Step 5: Update `generated_output_has_section_ordering` (line 435)**

The test searches for `"Hooks"` as a section comment position marker. The comment `# --- Hooks ---` is still emitted (standalone only), so this test should still pass. Verify by running it — if it passes, no change needed.

Run: `cargo test generated_output_has_section_ordering -- --nocapture`
Expected: PASS (the section comment is still emitted)

- [ ] **Step 6: Run the full test suite**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS

- [ ] **Step 7: Commit the test updates**

```bash
git add src/generator.rs
git commit -m "test: update hook tests for bind mount emission

Existing tests expected COPY-based hook patterns. Updated to assert
RUN --mount=type=bind output with mount target /frag-hooks.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 4: Add test — bind mount uses same resolution as tree COPY

**Files:**
- Modify: `src/generator.rs` (test module)

**Interfaces:**
- Consumes: `make_config_fragment` (pinned), `make_unpinned_config_fragment` (unpinned)
- Produces: test `bind_mount_uses_same_resolution_as_tree_copy`

- [ ] **Step 1: Write the test**

Add to the test module in `src/generator.rs`:

```rust
#[test]
fn bind_mount_uses_same_resolution_as_tree_copy() {
    // Pinned: both tree COPY and hook mount should use named stage
    let (mut cis, mf_cis) = make_config_fragment("cis", "bbb222");
    cis.manifest_index = 0;
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_cis],
    };
    let output = generate_containerfile(
        &manifest,
        &[cis],
        Some("sha256:base123"),
        &empty_dedup(),
        false,
    )
    .unwrap();
    assert!(
        output.contains("COPY --from=frag-cis /fragment/tree/ /"),
        "expected named stage in tree COPY:\n{}",
        output
    );
    assert!(
        output.contains("RUN --mount=type=bind,from=frag-cis,"),
        "expected named stage in hook mount:\n{}",
        output
    );

    // Unpinned: both tree COPY and hook mount should use inline image ref
    let (mut unpinned_cis, mf_unpinned) = make_unpinned_config_fragment("cis");
    unpinned_cis.manifest_index = 0;
    // Give it a tree path so it generates a tree COPY
    unpinned_cis
        .tree_paths
        .push(PathBuf::from("tree/usr/lib/sysctl.d/99-hardening.conf"));
    let unpinned_manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_unpinned],
    };
    let unpinned_output = generate_containerfile(
        &unpinned_manifest,
        &[unpinned_cis],
        None,
        &empty_dedup(),
        false,
    )
    .unwrap();
    assert!(
        unpinned_output.contains("COPY --from=quay.io/test/cis:2.1 /fragment/tree/ /"),
        "expected inline ref in tree COPY:\n{}",
        unpinned_output
    );
    assert!(
        unpinned_output.contains("RUN --mount=type=bind,from=quay.io/test/cis:2.1,"),
        "expected inline ref in hook mount:\n{}",
        unpinned_output
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test bind_mount_uses_same_resolution_as_tree_copy -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/generator.rs
git commit -m "test: verify bind mount from= matches tree COPY resolution

Ensures digest-pinned mode uses named stages and unpinned mode uses
inline image refs consistently across tree COPY and hook mount.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 5: Add test — mount options match MCO template

**Files:**
- Modify: `src/generator.rs` (test module)

**Interfaces:**
- Consumes: `make_unpinned_config_fragment`, `generate_containerfile`
- Produces: test `bind_mount_carries_mco_options`

- [ ] **Step 1: Write the test**

Add to the test module in `src/generator.rs`:

```rust
#[test]
fn bind_mount_carries_mco_options() {
    let (mut cis, mf_cis) = make_unpinned_config_fragment("cis");
    cis.manifest_index = 0;
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_cis],
    };

    // Standalone
    let standalone = generate_containerfile(
        &manifest,
        &[cis.clone()],
        None,
        &empty_dedup(),
        false,
    )
    .unwrap();
    assert!(
        standalone.contains("bind-propagation=rshared,z"),
        "standalone output must carry MCO mount options:\n{}",
        standalone
    );

    // OCP
    let ocp = generate_containerfile(
        &manifest,
        &[cis],
        None,
        &empty_dedup(),
        true,
    )
    .unwrap();
    assert!(
        ocp.contains("bind-propagation=rshared,z"),
        "OCP output must carry MCO mount options:\n{}",
        ocp
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test bind_mount_carries_mco_options -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/generator.rs
git commit -m "test: assert bind-propagation=rshared,z on all hook mounts

Mirrors MCO's on-cluster-build template mount options. Tests both
standalone and OCP output paths.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 6: Add test — standalone and OCP do not diverge in hook emission

**Files:**
- Modify: `src/generator.rs` (test module)

**Interfaces:**
- Consumes: `make_unpinned_config_fragment`, `generate_containerfile`
- Produces: test `standalone_and_ocp_emit_same_hook_instruction`

- [ ] **Step 1: Write the test**

Add to the test module in `src/generator.rs`:

```rust
#[test]
fn standalone_and_ocp_emit_same_hook_instruction() {
    let (mut cis, mf_cis) = make_unpinned_config_fragment("cis");
    cis.manifest_index = 0;
    cis.hook_paths = vec![
        PathBuf::from("10-configure.sh"),
        PathBuf::from("20-enable.sh"),
    ];
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![mf_cis],
    };

    let standalone = generate_containerfile(
        &manifest,
        &[cis.clone()],
        None,
        &empty_dedup(),
        false,
    )
    .unwrap();
    let ocp = generate_containerfile(
        &manifest,
        &[cis],
        None,
        &empty_dedup(),
        true,
    )
    .unwrap();

    // Extract the RUN --mount lines from each
    let standalone_mount: Vec<&str> = standalone
        .lines()
        .filter(|l| l.contains("--mount=type=bind") || l.trim_start().starts_with("/frag-hooks/"))
        .collect();
    let ocp_mount: Vec<&str> = ocp
        .lines()
        .filter(|l| l.contains("--mount=type=bind") || l.trim_start().starts_with("/frag-hooks/"))
        .collect();

    assert_eq!(
        standalone_mount, ocp_mount,
        "standalone and OCP hook emission must not diverge\nstandalone: {:?}\nocp: {:?}",
        standalone_mount, ocp_mount
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test standalone_and_ocp_emit_same_hook_instruction -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/generator.rs
git commit -m "test: verify standalone and OCP hook emission do not diverge

Both output paths must emit identical RUN --mount instructions for
hooks, differing only in comments and base FROM.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 7: Add test — one RUN --mount per fragment, not per hook

**Files:**
- Modify: `src/generator.rs` (test module)

**Interfaces:**
- Consumes: `make_unpinned_config_fragment`, `generate_containerfile`
- Produces: test `one_mount_per_fragment_not_per_hook`

- [ ] **Step 1: Write the test**

Add to the test module in `src/generator.rs`:

```rust
#[test]
fn one_mount_per_fragment_not_per_hook() {
    // Fragment with 3 hooks
    let mut loaded = LoadedFragment {
        fragment: Fragment {
            name: "multi-hook".to_string(),
            version: "1.0".into(),
            description: "test".into(),
            vendor: None,
            phase: FragmentPhase::Config,
            provides: FragmentProvides { repos: vec![] },
            packages: FragmentPackages { available: vec![] },
            conflicts: FragmentConflicts { fragments: vec![] },
        },
        tree_paths: vec![],
        hook_paths: vec![
            PathBuf::from("01-first.sh"),
            PathBuf::from("02-second.sh"),
            PathBuf::from("03-third.sh"),
        ],
        source: FragmentSource::Registry {
            image_ref: "quay.io/test/multi-hook:1.0".into(),
        },
        resolved_digest: None,
        manifest_index: 0,
        repo_file_contents: std::collections::HashMap::new(),
    };
    loaded.hook_paths.sort();
    let manifest_frag = ManifestFragment {
        image: "quay.io/test/multi-hook:1.0".into(),
        packages: vec![],
        mirror: None,
    };

    // Second fragment with 1 hook
    let loaded2 = LoadedFragment {
        fragment: Fragment {
            name: "single-hook".to_string(),
            version: "1.0".into(),
            description: "test".into(),
            vendor: None,
            phase: FragmentPhase::Config,
            provides: FragmentProvides { repos: vec![] },
            packages: FragmentPackages { available: vec![] },
            conflicts: FragmentConflicts { fragments: vec![] },
        },
        tree_paths: vec![],
        hook_paths: vec![PathBuf::from("setup.sh")],
        source: FragmentSource::Registry {
            image_ref: "quay.io/test/single-hook:1.0".into(),
        },
        resolved_digest: None,
        manifest_index: 1,
        repo_file_contents: std::collections::HashMap::new(),
    };
    let manifest_frag2 = ManifestFragment {
        image: "quay.io/test/single-hook:1.0".into(),
        packages: vec![],
        mirror: None,
    };

    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        fragments: vec![manifest_frag, manifest_frag2],
    };
    let output = generate_containerfile(
        &manifest,
        &[loaded, loaded2],
        None,
        &empty_dedup(),
        false,
    )
    .unwrap();

    // Two fragments with hooks -> exactly two RUN --mount instructions
    let mount_count = output.matches("RUN --mount=type=bind").count();
    assert_eq!(
        mount_count, 2,
        "expected 2 RUN --mount instructions (one per fragment), got {}:\n{}",
        mount_count, output
    );

    // First fragment's hooks all chained in one line
    assert!(
        output.contains("/frag-hooks/01-first.sh && /frag-hooks/02-second.sh && /frag-hooks/03-third.sh"),
        "expected all hooks chained with && in one instruction:\n{}",
        output
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test one_mount_per_fragment_not_per_hook -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/generator.rs
git commit -m "test: verify one RUN --mount per fragment, hooks chained

Two fragments with hooks produce exactly two mount instructions.
Hooks within a fragment are &&-chained in a single instruction.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 8: Run clippy and full test suite — final verification

**Files:**
- None (verification only)

**Interfaces:**
- Consumes: all changes from Tasks 1-7
- Produces: clean CI pass

- [ ] **Step 1: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: zero warnings

- [ ] **Step 2: Run full test suite**

Run: `cargo test -- --nocapture 2>&1`
Expected: all tests PASS

- [ ] **Step 3: If any failures, fix and commit**

Fix any issues found. If fixes are needed, commit them individually with descriptive messages.
