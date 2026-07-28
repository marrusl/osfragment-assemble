# Forced Package Installs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fragments declare `required` packages that always install during assembly, replacing the inert `available` field with enforced installs.

**Architecture:** Rename `FragmentPackages.available` to `required` with `#[serde(deny_unknown_fields)]` for safe migration. The generator collects required packages (in fragment order: phase weight then manifest position) before manifest-selected packages, deduplicating via `HashSet` with first-seen-wins. The OCI annotation key changes from `io.bootc.fragment.packages.available` to `io.bootc.fragment.packages.required`.

**Tech Stack:** Rust, serde, toml, serde_json

## Global Constraints

- `#[serde(deny_unknown_fields)]` on `FragmentPackages` -- unknown keys (including `available`) are hard parse errors.
- Collection order: fragment-declared `required` first (phase weight, then manifest order), then manifest-selected `packages`.
- Dedup: exact string match, `HashSet`, first-seen wins. Cross-fragment duplicates are silent.
- Old annotation key `io.bootc.fragment.packages.available` is never read. No alias kept.
- `postgresql` keeps `phase = "repos"` -- forced packages are not fragment payload.
- Output remains a single batched `dnf install`.

---

### Task 1: Rename `FragmentPackages.available` to `required` with `deny_unknown_fields`

**Files:**
- Modify: `src/fragment.rs:27-31` (struct definition)
- Modify: `src/fragment.rs:124-151` (test constants MINIMAL_TOML, FULL_TOML)
- Modify: `src/fragment.rs:154-170` (tests that assert `.available`)

**Interfaces:**
- Consumes: nothing
- Produces: `FragmentPackages { required: Vec<String> }` with `#[serde(deny_unknown_fields)]`

- [ ] **Step 1: Update `FragmentPackages` struct**

Replace the struct definition at line 27:

```rust
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FragmentPackages {
    #[serde(default)]
    pub required: Vec<String>,
}
```

- [ ] **Step 2: Update test constant `FULL_TOML`**

Replace `available = ["tailscale"]` with `required = ["tailscale"]` in the `FULL_TOML` constant at line 146-147:

```rust
    const FULL_TOML: &str = r#"
[fragment]
name = "tailscale"
version = "1.82.0"
description = "Tailscale VPN client"
vendor = "Tailscale Inc."
phase = "config"

[fragment.provides]
repos = ["tailscale-stable"]

[fragment.packages]
required = ["tailscale"]

[fragment.conflicts]
fragments = []
"#;
```

- [ ] **Step 3: Update test assertions from `.available` to `.required`**

In `parse_minimal_fragment` (line 161):
```rust
        assert!(frag.packages.required.is_empty());
```

In `parse_full_fragment` (line 170):
```rust
        assert_eq!(frag.packages.required, vec!["tailscale"]);
```

- [ ] **Step 4: Add test for `deny_unknown_fields` rejecting `available`**

Add after the `reject_invalid_phase` test:

```rust
    #[test]
    fn reject_unknown_packages_field() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "unknown field test"
phase = "config"

[fragment.packages]
available = ["grafana"]
"#;
        let err = parse_fragment_toml(bad).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' error, got: {}",
            msg
        );
    }

    #[test]
    fn reject_typo_in_packages_field() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "typo field test"
phase = "config"

[fragment.packages]
requred = ["grafana"]
"#;
        let err = parse_fragment_toml(bad).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' error, got: {}",
            msg
        );
    }
```

- [ ] **Step 5: Run tests to verify**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib fragment::tests
```

Expect: `parse_all_example_fragments` fails (examples still use `available`). All other tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/fragment.rs
git commit -m "refactor: rename FragmentPackages.available to required with deny_unknown_fields

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 2: Update all references to `.available` across the codebase

**Files:**
- Modify: `src/inspect.rs:72-73` (display output)
- Modify: `src/loader.rs:233-237` (annotation read)
- Modify: `src/loader.rs:244-253` (Fragment construction from annotations)
- Modify: `src/validate.rs:133` (test helper `test_fragment`)
- Modify: `src/generator.rs:356` (test helper `make_repos_fragment`)
- Modify: `src/generator.rs:389` (test helper `make_config_fragment`)
- Modify: `src/generator.rs:677` (test helper `make_unpinned_repos_fragment`)
- Modify: `src/generator.rs:709` (test helper `make_unpinned_config_fragment`)
- Modify: `src/generator.rs:898` (test fixture `multiple_hooks_run_in_alphabetical_order`)

**Interfaces:**
- Consumes: `FragmentPackages { required }` from Task 1
- Produces: all code compiles with the renamed field

- [ ] **Step 1: Update `inspect.rs` display**

At line 72-73, change:
```rust
    if !fragment.packages.required.is_empty() {
        println!("Packages: {}", fragment.packages.required.join(", "));
    }
```

- [ ] **Step 2: Update `loader.rs` annotation key and field name**

At line 233-237, change the annotation key:
```rust
    let required: Vec<String> = annotations
        .get("io.bootc.fragment.packages.required")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
```

At line 244-253, change the struct field in the `Fragment` construction:
```rust
    Ok(Some(Fragment {
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        vendor,
        phase,
        provides: FragmentProvides { repos },
        packages: FragmentPackages { required },
        conflicts: FragmentConflicts { fragments: vec![] },
    }))
```

- [ ] **Step 3: Update generator test helpers and fixtures**

In `make_repos_fragment` at line 356:
```rust
                packages: FragmentPackages { required: vec![] },
```

In `make_config_fragment` at line 389:
```rust
                packages: FragmentPackages { required: vec![] },
```

In `make_unpinned_repos_fragment` at line 677:
```rust
                packages: FragmentPackages { required: vec![] },
```

In `make_unpinned_config_fragment` at line 709:
```rust
                packages: FragmentPackages { required: vec![] },
```

In `multiple_hooks_run_in_alphabetical_order` test at line 898:
```rust
                packages: FragmentPackages { required: vec![] },
```

- [ ] **Step 4: Update validate.rs test helper**

In `test_fragment` helper at line 133:
```rust
                packages: FragmentPackages { required: vec![] },
```

- [ ] **Step 5: Search for any remaining `available` references**

```bash
cd /Users/mrussell/Work/osfragment-assemble
grep -rn '\.available' src/
grep -rn 'packages.available' src/
grep -rn 'available' src/ | grep -v 'required'
```

The third grep catches struct literal field initializers (`available: vec![]`) that the dot-prefixed patterns miss.

Expect: zero hits from all three commands.

- [ ] **Step 6: Run full test suite**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test 2>&1
```

Expect: all tests pass except `parse_all_example_fragments` (examples still use `available`).

- [ ] **Step 7: Commit**

```bash
git add src/inspect.rs src/loader.rs src/generator.rs src/validate.rs
git commit -m "refactor: update all .available references to .required across codebase

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 3: Collect fragment-declared required packages in the generator

**Files:**
- Modify: `src/generator.rs:227-251` (package collection block)

**Interfaces:**
- Consumes: `LoadedFragment.fragment.packages.required`, `ManifestFragment.packages`, fragment ordering (phase weight + manifest index)
- Produces: merged, deduplicated package list in the `dnf install` output

- [ ] **Step 1: Write failing test -- required packages appear without manifest entry**

Add in `src/generator.rs` tests module:

```rust
    #[test]
    fn required_packages_appear_without_manifest_entry() {
        let (mut epel, mut mf_epel) = make_repos_fragment("epel", "aaa111");
        epel.fragment.packages.required = vec!["htop".into(), "tmux".into()];
        mf_epel.packages = vec![]; // no manifest-selected packages
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(
            &manifest,
            &[epel],
            Some("sha256:base123"),
            &empty_dedup(),
            false,
        )
        .unwrap();
        assert!(
            output.contains("htop") && output.contains("tmux"),
            "expected required packages in output:\n{}",
            output
        );
        assert!(
            output.contains("dnf install -y"),
            "expected dnf install line in output:\n{}",
            output
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib generator::tests::required_packages_appear_without_manifest_entry
```

Expect: FAIL (current code only collects from `manifest.fragments[].packages`).

- [ ] **Step 3: Write failing test -- required before manifest-selected, deduplication**

```rust
    #[test]
    fn required_packages_before_manifest_selected_with_dedup() {
        let (mut epel, mut mf_epel) = make_repos_fragment("epel", "aaa111");
        epel.fragment.packages.required = vec!["htop".into()];
        mf_epel.packages = vec!["htop".into(), "jq".into()]; // htop duplicated
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(
            &manifest,
            &[epel],
            Some("sha256:base123"),
            &empty_dedup(),
            false,
        )
        .unwrap();
        // htop should appear exactly once
        let htop_count = output.matches("htop").count();
        assert_eq!(
            htop_count, 1,
            "htop should appear exactly once (dedup), found {} in:\n{}",
            htop_count, output
        );
        // Both htop and jq should be present
        assert!(output.contains("jq"), "expected jq in output:\n{}", output);
        // htop (required) should appear before jq (manifest-selected)
        let htop_pos = output.find("htop").unwrap();
        let jq_pos = output.find("jq").unwrap();
        assert!(
            htop_pos < jq_pos,
            "required htop should appear before manifest-selected jq"
        );
    }
```

- [ ] **Step 4: Write failing test -- cross-fragment required dedup is silent**

```rust
    #[test]
    fn cross_fragment_required_dedup_silent() {
        let (mut epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        epel.fragment.packages.required = vec!["shared-dep".into()];
        let (mut cis, mf_cis) = make_config_fragment("cis", "bbb222");
        cis.fragment.packages.required = vec!["shared-dep".into()];
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
        let dep_count = output.matches("shared-dep").count();
        assert_eq!(
            dep_count, 1,
            "shared-dep should appear exactly once, found {} in:\n{}",
            dep_count, output
        );
    }
```

- [ ] **Step 5: Write failing test -- repos-phase required before config-phase required**

```rust
    #[test]
    fn required_packages_ordered_by_phase_weight() {
        // Config fragment listed first in manifest, repos fragment second.
        // Required packages from repos fragment should still come first
        // because phase weight (repos=10) < (config=30).
        let (mut cis, mf_cis) = make_config_fragment("cis", "bbb222");
        cis.fragment.packages.required = vec!["config-pkg".into()];
        cis.manifest_index = 0;
        let (mut epel, mf_epel) = make_repos_fragment("epel", "aaa111");
        epel.fragment.packages.required = vec!["repos-pkg".into()];
        epel.manifest_index = 1;
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_cis, mf_epel],
        };
        // fragments sorted by phase weight: epel (repos=10) before cis (config=30)
        let sorted_fragments = vec![epel, cis];
        let output = generate_containerfile(
            &manifest,
            &sorted_fragments,
            Some("sha256:base123"),
            &empty_dedup(),
            false,
        )
        .unwrap();
        let repos_pos = output.find("repos-pkg").unwrap();
        let config_pos = output.find("config-pkg").unwrap();
        assert!(
            repos_pos < config_pos,
            "repos-phase required should appear before config-phase required"
        );
    }
```

- [ ] **Step 6: Run tests to verify they fail**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib generator::tests::required_packages 2>&1
cargo test --lib generator::tests::cross_fragment_required 2>&1
```

Expect: all four new tests FAIL.

- [ ] **Step 7: Implement new package collection logic**

Replace the package collection block in `generate_containerfile` (lines 227-251):

```rust
    // Packages — required (fragment-declared) first, then manifest-selected
    let mut seen = std::collections::HashSet::new();
    let mut all_packages: Vec<String> = Vec::new();

    // Phase 1: fragment-declared required packages, in fragment order
    // (fragments are already sorted by phase weight then manifest order)
    for loaded in fragments {
        for pkg in &loaded.fragment.packages.required {
            if seen.insert(pkg.clone()) {
                all_packages.push(pkg.clone());
            }
        }
    }

    // Phase 2: manifest-selected packages, in manifest order
    for mf in &manifest.fragments {
        for pkg in &mf.packages {
            if seen.insert(pkg.clone()) {
                all_packages.push(pkg.clone());
            }
        }
    }

    if !all_packages.is_empty() {
        if !ocp {
            writeln!(out, "# --- Packages ---")?;
        }
        writeln!(out, "RUN dnf install -y \\")?;
        for pkg in &all_packages {
            writeln!(out, "        {} \\", pkg)?;
        }
        writeln!(out, "    && dnf clean all \\")?;
        writeln!(
            out,
            "    && rm -rf /var/log/dnf* /var/log/hawkey.log /var/lib/dnf/history.sqlite*"
        )?;
        if !ocp {
            writeln!(out)?;
        }
    }
```

- [ ] **Step 8: Run all tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib generator::tests
```

Expect: all tests pass (including existing ones that rely on manifest `packages`).

- [ ] **Step 9: Commit**

```bash
git add src/generator.rs
git commit -m "feat: collect fragment-declared required packages in generator

Required packages from fragments are collected first (phase weight order),
then manifest-selected packages. Dedup via HashSet, first-seen wins.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 4: Write test for manifest-selected package with no fragment declaration

**Files:**
- Modify: `src/generator.rs` (test module)

**Interfaces:**
- Consumes: generator logic from Task 3
- Produces: test coverage for acceptance criterion "a manifest may select a package no fragment declares"

- [ ] **Step 1: Write test**

```rust
    #[test]
    fn manifest_selected_package_no_fragment_declaration() {
        let (epel, mut mf_epel) = make_repos_fragment("epel", "aaa111");
        mf_epel.packages = vec!["unrelated-tool".into()];
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![mf_epel],
        };
        let output = generate_containerfile(
            &manifest,
            &[epel],
            Some("sha256:base123"),
            &empty_dedup(),
            false,
        )
        .unwrap();
        assert!(
            output.contains("unrelated-tool"),
            "manifest-selected package should appear even when no fragment declares it:\n{}",
            output
        );
    }
```

- [ ] **Step 2: Run test to verify it passes**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib generator::tests::manifest_selected_package_no_fragment_declaration
```

Expect: PASS (this was already working; the test documents the acceptance criterion).

- [ ] **Step 3: Commit**

```bash
git add src/generator.rs
git commit -m "test: add coverage for manifest-selected packages without fragment declaration

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 5: Migrate example fragments

**Files:**
- Modify: `examples/fragments/grafana/fragment.toml`
- Modify: `examples/fragments/nginx/fragment.toml`
- Modify: `examples/fragments/node-exporter/fragment.toml`
- Modify: `examples/fragments/tailscale/fragment.toml`
- Modify: `examples/fragments/postgresql/fragment.toml`
- Modify: `examples/fragments/epel/fragment.toml`
- Modify: `examples/fragments/hashicorp/fragment.toml`

**Interfaces:**
- Consumes: `FragmentPackages` with `deny_unknown_fields` and `required` field from Task 1
- Produces: all example fragments parse successfully

- [ ] **Step 1: Migrate `grafana` -- `available` becomes `required`**

```toml
[fragment]
name = "grafana"
version = "11.0"
description = "Grafana monitoring dashboard — repo, package, and service enablement"
vendor = "Grafana Labs"
phase = "config"

[fragment.provides]
repos = ["grafana"]

[fragment.packages]
required = ["grafana"]
```

- [ ] **Step 2: Migrate `nginx` -- `available` becomes `required`**

```toml
[fragment]
name = "nginx"
version = "1.26"
description = "Nginx web server — official repo, package, and service enablement"
vendor = "F5 / Nginx Inc."
phase = "config"

[fragment.provides]
repos = ["nginx-stable"]

[fragment.packages]
required = ["nginx"]
```

- [ ] **Step 3: Migrate `node-exporter` -- `available` becomes `required`**

```toml
[fragment]
name = "node-exporter"
version = "1.8.0"
description = "Prometheus Node Exporter — self-contained with EPEL repo"
phase = "config"

[fragment.provides]
repos = ["epel"]

[fragment.packages]
required = ["node-exporter"]
```

- [ ] **Step 4: Migrate `tailscale` -- `available` becomes `required`**

```toml
[fragment]
name = "tailscale"
version = "1.82.0"
description = "Tailscale VPN client — repo, packages, and service enablement"
vendor = "Tailscale Inc."
phase = "config"

[fragment.provides]
repos = ["tailscale-stable"]

[fragment.packages]
required = ["tailscale"]
```

- [ ] **Step 5: Migrate `postgresql` -- force postgresql17 only, drop postgresql16**

```toml
[fragment]
name = "postgresql"
version = "17"
description = "PostgreSQL Global Development Group RPM repository"
vendor = "PostgreSQL Global Development Group"
phase = "repos"

[fragment.provides]
repos = ["pgdg-common", "pgdg17"]

[fragment.packages]
required = [
    "postgresql17-server",
    "postgresql17",
]
```

- [ ] **Step 6: Migrate `epel` -- remove `[fragment.packages]` section entirely**

```toml
[fragment]
name = "epel"
version = "10"
description = "Extra Packages for Enterprise Linux (EPEL) repository"
vendor = "Fedora Project"
phase = "repos"

[fragment.provides]
repos = ["epel"]
```

- [ ] **Step 7: Migrate `hashicorp` -- remove `[fragment.packages]` section entirely**

```toml
[fragment]
name = "hashicorp"
version = "1.0"
description = "HashiCorp Vault — repo, package, and basic configuration"
vendor = "HashiCorp"
phase = "config"

[fragment.provides]
repos = ["hashicorp"]
```

- [ ] **Step 8: Verify `cis-hardening` needs no changes**

`cis-hardening` has no `[fragment.packages]` section. No change needed.

- [ ] **Step 9: Run `parse_all_example_fragments` test**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib fragment::tests::parse_all_example_fragments
```

Expect: PASS.

- [ ] **Step 10: Run full test suite**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test 2>&1
```

Expect: all tests pass.

- [ ] **Step 11: Commit**

```bash
git add examples/
git commit -m "feat: migrate example fragments from available to required packages

grafana, nginx, node-exporter, tailscale, postgresql force their packages.
epel and hashicorp drop their lists (catalogue fragments, consumers select
in manifest). postgresql drops postgresql16 entries.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 6: Update `docs/fragment-format.md`

**Files:**
- Modify: `docs/fragment-format.md`

**Interfaces:**
- Consumes: renamed annotation key, renamed field
- Produces: updated documentation

- [ ] **Step 1: Update the OCI annotation key**

In the "OCI Annotations (Fast-Path Metadata)" section, change:
```
- `io.bootc.fragment.packages.available`: JSON array of package names
```
to:
```
- `io.bootc.fragment.packages.required`: JSON array of required package names
```

- [ ] **Step 2: Update the `podman build --annotation` example**

Add the `required` annotation to the example:
```bash
podman build --annotation io.bootc.fragment.name=tailscale \
             --annotation io.bootc.fragment.version=1.82.0 \
             --annotation io.bootc.fragment.phase=config \
             --annotation 'io.bootc.fragment.packages.required=["tailscale"]' \
             -f Containerfile.fragment -t quay.io/user/tailscale:1.82.0 .
```

- [ ] **Step 3: Update the Field Constraints section**

Change the `packages.available` constraint description:
```
- `packages.required`: Optional array of package names this fragment forces during assembly. These packages are always installed, even without a manifest entry. Unknown keys in `[fragment.packages]` are rejected as parse errors.
```

- [ ] **Step 4: Update the `fragment.toml` Schema section**

In the schema table/example, change `available` to `required` wherever it appears.

- [ ] **Step 5: Commit**

```bash
git add docs/fragment-format.md
git commit -m "docs: update fragment-format.md for available-to-required rename

Annotation key, field constraints, schema example, and podman build
example all updated.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 7: Add integration-level test for postgresql forced packages

**Files:**
- Modify: `src/generator.rs` (test module)
- Modify: `src/fragment.rs` (test module, phase verification against real example)

**Interfaces:**
- Consumes: generator logic from Task 3, `FragmentPackages.required`, migrated examples from Task 5
- Produces: test coverage for "postgresql emits postgresql17-server and postgresql17 with no manifest entry, and keeps phase = repos"

- [ ] **Step 1: Write test**

```rust
    #[test]
    fn postgresql_forces_packages_as_repos_phase() {
        let (mut pg, mf_pg) = make_repos_fragment("postgresql", "pg1234");
        pg.fragment.packages.required =
            vec!["postgresql17-server".into(), "postgresql17".into()];
        pg.fragment.provides.repos = vec!["pgdg-common".into(), "pgdg17".into()];
        // manifest selects no packages for postgresql
        let manifest = Manifest {
            base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
            fragments: vec![ManifestFragment {
                image: "quay.io/test/postgresql:17".into(),
                packages: vec![],
                mirror: None,
            }],
        };
        let output = generate_containerfile(
            &manifest,
            &[pg],
            Some("sha256:base123"),
            &empty_dedup(),
            false,
        )
        .unwrap();
        assert!(
            output.contains("postgresql17-server"),
            "expected postgresql17-server in output:\n{}",
            output
        );
        assert!(
            output.contains("postgresql17"),
            "expected postgresql17 in output:\n{}",
            output
        );
        assert!(
            output.contains("dnf install -y"),
            "expected dnf install in output:\n{}",
            output
        );
    }
```

- [ ] **Step 2: Write test -- postgresql example preserves phase = repos after migration**

The generator test above uses a synthetic `make_repos_fragment` helper, so it proves package emission but never reads the real migrated example. This test parses the actual `examples/fragments/postgresql/fragment.toml` and verifies the phase field survived migration intact.

Add in `src/fragment.rs` tests module, after `parse_all_example_fragments`:

```rust
    #[test]
    fn postgresql_example_preserves_repos_phase() {
        let toml_path = Path::new("examples/fragments/postgresql/fragment.toml");
        let content = std::fs::read_to_string(toml_path)
            .expect("postgresql example fragment should exist");
        let frag = parse_fragment_toml(&content)
            .expect("postgresql example fragment should parse");
        assert_eq!(
            frag.phase,
            FragmentPhase::Repos,
            "postgresql must stay phase=repos after migration to required packages"
        );
        assert!(
            frag.packages.required.contains(&"postgresql17-server".to_string()),
            "postgresql must declare postgresql17-server as required"
        );
        assert!(
            frag.packages.required.contains(&"postgresql17".to_string()),
            "postgresql must declare postgresql17 as required"
        );
    }
```

- [ ] **Step 3: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test --lib generator::tests::postgresql_forces_packages_as_repos_phase
cargo test --lib fragment::tests::postgresql_example_preserves_repos_phase
```

Expect: both PASS.

- [ ] **Step 4: Commit**

```bash
git add src/generator.rs src/fragment.rs
git commit -m "test: add integration tests for postgresql forced packages

Generator test verifies postgresql17-server and postgresql17 appear in
dnf install output with no manifest entry. Fragment test parses the real
example and asserts phase=repos and required packages survived migration.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 8: Verify no regressions and run clippy

**Files:**
- No modifications

**Interfaces:**
- Consumes: all prior tasks
- Produces: clean test suite, clean clippy

- [ ] **Step 1: Run full test suite**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo test 2>&1
```

Expect: all tests pass.

- [ ] **Step 2: Run clippy with deny warnings**

```bash
cd /Users/mrussell/Work/osfragment-assemble
cargo clippy -- -D warnings 2>&1
```

Expect: zero warnings.

- [ ] **Step 3: Verify no remaining references to old field or annotation**

```bash
cd /Users/mrussell/Work/osfragment-assemble
grep -rn 'packages\.available' src/ examples/ docs/
grep -rn 'packages.available' src/ examples/ docs/
```

Expect: zero hits.

---

## Self-Review Checklist

### Spec coverage

| Acceptance criterion | Task |
|---|---|
| Fragment declaring `required` produces packages in batched install with no manifest entry | Task 3 (test + impl) |
| Forced and selected deduplicate, exact-string, first-seen wins, required before selected | Task 3 (test + impl) |
| Two fragments requiring same package: one entry, no warning | Task 3 (test) |
| Manifest may select a package no fragment declares, not an error | Task 4 (test) |
| `postgresql` emits postgresql17-server and postgresql17 with no manifest entry, keeps `phase = "repos"` | Task 7 (generator test for emission, fragment test for real example phase + packages) |
| `available` or unknown key in `[fragment.packages]` is hard parse error | Task 1 (tests) |
| OCI annotation emitted and read as `io.bootc.fragment.packages.required`; old key not read | Task 2 (loader change) |
| Example fragments updated | Task 5 (all 8 fragments covered) |
| Docs updated (annotation key list, `podman build` example, schema) | Task 6 |

### Placeholder scan

No steps contain TBD, TODO, or "implement later". All steps have concrete code or commands.

### Type consistency

- `FragmentPackages { required: Vec<String> }` -- used consistently across fragment.rs, loader.rs, inspect.rs, generator.rs, validate.rs, and all test helpers (including `make_unpinned_repos_fragment`, `make_unpinned_config_fragment`, `multiple_hooks_run_in_alphabetical_order`).
- `#[serde(deny_unknown_fields)]` only on `FragmentPackages` -- other structs unchanged.

### Example migration

| Fragment | Action | Task |
|---|---|---|
| grafana | `available` -> `required` | Task 5, Step 1 |
| nginx | `available` -> `required` | Task 5, Step 2 |
| node-exporter | `available` -> `required` | Task 5, Step 3 |
| tailscale | `available` -> `required` | Task 5, Step 4 |
| postgresql | `available` -> `required`, drop pg16 entries | Task 5, Step 5 |
| epel | Remove `[fragment.packages]` section | Task 5, Step 6 |
| hashicorp | Remove `[fragment.packages]` section | Task 5, Step 7 |
| cis-hardening | No change needed (no packages section) | Task 5, Step 8 |
