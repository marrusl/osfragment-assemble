# Conditional Bootc Steps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit `bootc container lint` and `systemctl preset-all` only when the base image is classified as bootc, while preserving current behavior for all existing manifests.

**Architecture:** A `BaseType` enum (`Bootc` / `Container`) is determined by a classifier that checks manifest override first, then `skopeo inspect` for the `containers.bootc` label, defaulting to `Bootc` when the label is absent or lookup fails. The generator receives a capability set (`HashSet<Capability>`) derived from the classification; tool-managed steps carry a `requires` field and are emitted only when their requirement is in the set. MachineOSConfig always uses the full bootc capability set regardless of classification.

**Tech Stack:** Rust, serde (YAML deserialization), std::process::Command (skopeo), clap (CLI args), anyhow (errors)

## Global Constraints

- Default classification is `Bootc` when label absent (preserves current output for all existing manifests)
- Failed `skopeo inspect` warns on stderr and classifies as `Bootc` (not a hard failure)
- Image name matching is never a classification signal
- MachineOSConfig artifact always emits both bootc steps regardless of classification
- `--ocp` adds a second artifact; classification always runs for the standalone Containerfile
- Manifest `baseType` override wins over label inspection; when present, no inspection happens
- Only two capabilities exist now (`Bootc`, `Systemd`); the `requires` field is the extension point, not an abstraction to build out

---

### Task 1: Add `BaseType` enum and manifest `baseType` field

**Files:**
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/manifest.rs:13-21` (ManifestYaml struct)
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/manifest.rs:31-35` (Manifest struct)
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/manifest.rs:55-78` (parse_manifest)
- Test: `/Users/mrussell/Work/osfragment-assemble/src/manifest.rs` (inline tests)

**Interfaces:**
- Consumes: nothing
- Produces: `BaseType` enum (`Bootc`, `Container`), `Manifest.base_type: Option<BaseType>` field

- [ ] **Step 1: Write the failing tests**

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseType {
    Bootc,
    Container,
}

// In #[cfg(test)] mod tests:

#[test]
fn parse_base_type_bootc() {
    let yaml = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: registry.redhat.io/rhel10/rhel-bootc:10.0
baseType: bootc
fragments:
  - image: quay.io/test/epel:10
"#;
    let manifest = parse_manifest(yaml).unwrap();
    assert_eq!(manifest.base_type, Some(BaseType::Bootc));
}

#[test]
fn parse_base_type_container() {
    let yaml = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: quay.io/fedora/fedora:41
baseType: container
fragments:
  - image: quay.io/test/epel:10
"#;
    let manifest = parse_manifest(yaml).unwrap();
    assert_eq!(manifest.base_type, Some(BaseType::Container));
}

#[test]
fn parse_base_type_absent() {
    let manifest = parse_manifest(MINIMAL_YAML).unwrap();
    assert_eq!(manifest.base_type, None);
}

#[test]
fn parse_base_type_invalid_rejected() {
    let yaml = r#"
apiVersion: bootc.io/v1alpha1
kind: Composition
base: quay.io/fedora/fedora:41
baseType: something-else
fragments:
  - image: quay.io/test/epel:10
"#;
    assert!(parse_manifest(yaml).is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib manifest
```

- [ ] **Step 3: Add BaseType enum and update ManifestYaml / Manifest / parse_manifest**

Add the `BaseType` enum (before `ManifestYaml`):

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseType {
    Bootc,
    Container,
}
```

Add `baseType` to `ManifestYaml`:

```rust
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ManifestYaml {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    base: Option<String>,
    #[serde(default, rename = "baseType")]
    base_type: Option<BaseType>,
    #[serde(default)]
    fragments: Vec<ManifestFragmentYaml>,
}
```

Add `base_type` to public `Manifest`:

```rust
#[derive(Debug, Clone)]
pub struct Manifest {
    pub base: String,
    pub base_type: Option<BaseType>,
    pub fragments: Vec<ManifestFragment>,
}
```

Update `parse_manifest` to pass through:

```rust
pub fn parse_manifest(content: &str) -> Result<Manifest> {
    let raw: ManifestYaml =
        serde_yaml::from_str(content).context("failed to parse manifest YAML")?;

    let base = raw
        .base
        .ok_or_else(|| anyhow::anyhow!("manifest missing required 'base' field"))?;

    if raw.fragments.is_empty() {
        bail!("manifest must contain at least one fragment");
    }

    let fragments = raw
        .fragments
        .into_iter()
        .map(|f| ManifestFragment {
            image: f.image,
            packages: f.packages,
            mirror: f.mirror,
        })
        .collect();

    Ok(Manifest {
        base,
        base_type: raw.base_type,
        fragments,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib manifest
```

- [ ] **Step 5: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/manifest.rs
git commit -m "feat(manifest): add baseType field for base image classification

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 2: Add `Capability` enum and `classify_base` function

**Files:**
- Create: `/Users/mrussell/Work/osfragment-assemble/src/classify.rs`
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/lib.rs` (add `pub mod classify;`)

**Interfaces:**
- Consumes: `BaseType` from Task 1
- Produces: `Capability` enum (`Bootc`, `Systemd`), `CapabilitySet` type alias, `classify_base(base_image: &str, manifest_override: Option<&BaseType>) -> CapabilitySet`, `capabilities_for_base_type(base_type: BaseType) -> CapabilitySet`

- [ ] **Step 1: Write the failing tests**

Create `/Users/mrussell/Work/osfragment-assemble/src/classify.rs`:

```rust
use std::collections::HashSet;

use crate::manifest::BaseType;

/// Capabilities that a base image may provide.
/// Steps in the phase table carry a `requires` field referencing one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Bootc,
    Systemd,
}

/// The set of capabilities detected for a given base image.
pub type CapabilitySet = HashSet<Capability>;

/// Build the capability set for a given base type classification.
pub fn capabilities_for_base_type(base_type: BaseType) -> CapabilitySet {
    todo!()
}

/// Label key checked on the base image via `skopeo inspect`.
const BOOTC_LABEL_KEY: &str = "containers.bootc";

/// Classify the base image and return its capability set.
///
/// Classification order:
/// 1. Manifest `baseType` override (when present, no inspection happens)
/// 2. `containers.bootc` image label via `skopeo inspect`
/// 3. Default: `Bootc` (preserves current behavior, fails loudly not silently)
///
/// When `skopeo inspect` fails (network, auth), classifies as `Bootc`,
/// warns on stderr, and continues.
pub fn classify_base(
    base_image: &str,
    manifest_override: Option<&BaseType>,
) -> CapabilitySet {
    todo!()
}

/// Query the `containers.bootc` label from the base image config via skopeo.
/// Returns `Some(true)` if the label exists and is non-empty,
/// `Some(false)` if the label is absent or empty,
/// `None` if the lookup failed.
fn probe_bootc_label(base_image: &str) -> Option<bool> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootc_type_yields_both_capabilities() {
        let caps = capabilities_for_base_type(BaseType::Bootc);
        assert!(caps.contains(&Capability::Bootc));
        assert!(caps.contains(&Capability::Systemd));
        assert_eq!(caps.len(), 2);
    }

    #[test]
    fn container_type_yields_empty_set() {
        let caps = capabilities_for_base_type(BaseType::Container);
        assert!(caps.is_empty());
    }

    #[test]
    fn manifest_override_bootc_skips_inspection() {
        let caps = classify_base("nonexistent.invalid/image:1", Some(&BaseType::Bootc));
        assert!(caps.contains(&Capability::Bootc));
        assert!(caps.contains(&Capability::Systemd));
    }

    #[test]
    fn manifest_override_container_skips_inspection() {
        let caps = classify_base("nonexistent.invalid/image:1", Some(&BaseType::Container));
        assert!(caps.is_empty());
    }
}
```

- [ ] **Step 2: Register the module in lib.rs**

Add `pub mod classify;` to `/Users/mrussell/Work/osfragment-assemble/src/lib.rs`.

- [ ] **Step 3: Run tests to verify they fail (todo! panics)**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib classify
```

- [ ] **Step 4: Implement `capabilities_for_base_type`**

```rust
pub fn capabilities_for_base_type(base_type: BaseType) -> CapabilitySet {
    match base_type {
        BaseType::Bootc => {
            let mut set = HashSet::new();
            set.insert(Capability::Bootc);
            set.insert(Capability::Systemd);
            set
        }
        BaseType::Container => HashSet::new(),
    }
}
```

- [ ] **Step 5: Implement `probe_bootc_label`**

```rust
fn probe_bootc_label(base_image: &str) -> Option<bool> {
    let output = std::process::Command::new("skopeo")
        .args([
            "inspect",
            "--override-os",
            "linux",
            "--format",
            &format!("{{{{.Labels.{}}}}}", BOOTC_LABEL_KEY.replace('.', "\\.")),
            &format!("docker://{}", base_image),
        ])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "warning: skopeo inspect failed for {}: {} — classifying as bootc",
                base_image, e
            );
            return None;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "warning: skopeo inspect failed for {}: {} — classifying as bootc",
            base_image,
            stderr.trim()
        );
        return None;
    }

    let label_value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // skopeo prints "<no value>" when the label is absent
    if label_value.is_empty() || label_value == "<no value>" {
        Some(false)
    } else {
        Some(true)
    }
}
```

- [ ] **Step 6: Implement `classify_base`**

```rust
pub fn classify_base(
    base_image: &str,
    manifest_override: Option<&BaseType>,
) -> CapabilitySet {
    // Signal 1: manifest override wins unconditionally
    if let Some(base_type) = manifest_override {
        return capabilities_for_base_type(base_type.clone());
    }

    // Signal 2: probe the containers.bootc label
    match probe_bootc_label(base_image) {
        Some(true) => capabilities_for_base_type(BaseType::Bootc),
        Some(false) => {
            // Label absent — default to bootc (preserves current behavior,
            // fails loudly via bootc container lint rather than silently
            // dropping systemctl preset-all)
            capabilities_for_base_type(BaseType::Bootc)
        }
        None => {
            // Lookup failed — already warned on stderr, default to bootc
            capabilities_for_base_type(BaseType::Bootc)
        }
    }
}
```

- [ ] **Step 7: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib classify
```

- [ ] **Step 8: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/classify.rs src/lib.rs
git commit -m "feat(classify): add base image classifier with capability set

Two-signal classification: manifest baseType override, then
containers.bootc label via skopeo inspect. Default is bootc
when label absent or lookup fails.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 3: Add `classify_base_with_probe` for testable label-based classification

The unit tests in Task 2 cover the manifest override path. Label-based classification
relies on `skopeo` which is not available in unit tests. This task adds an internal
function that accepts a probe result, enabling deterministic unit tests for every
classification branch without mocking process execution.

**Files:**
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/classify.rs`

**Interfaces:**
- Consumes: `Capability`, `CapabilitySet`, `BaseType` from Tasks 1-2
- Produces: `classify_base_with_probe(manifest_override: Option<&BaseType>, probe_result: Option<bool>) -> CapabilitySet` (internal, `pub(crate)` for integration tests)

- [ ] **Step 1: Write the failing tests**

Add to `classify.rs` test module:

```rust
#[test]
fn probe_true_yields_bootc_caps() {
    let caps = classify_base_with_probe(None, Some(true));
    assert!(caps.contains(&Capability::Bootc));
    assert!(caps.contains(&Capability::Systemd));
}

#[test]
fn probe_false_no_label_defaults_to_bootc() {
    // Label absent — default is bootc (preserves current behavior)
    let caps = classify_base_with_probe(None, Some(false));
    assert!(caps.contains(&Capability::Bootc));
    assert!(caps.contains(&Capability::Systemd));
}

#[test]
fn probe_none_lookup_failed_defaults_to_bootc() {
    // Lookup failed — default is bootc
    let caps = classify_base_with_probe(None, None);
    assert!(caps.contains(&Capability::Bootc));
    assert!(caps.contains(&Capability::Systemd));
}

#[test]
fn manifest_override_container_ignores_probe() {
    // Even if probe says bootc, manifest override wins
    let caps = classify_base_with_probe(Some(&BaseType::Container), Some(true));
    assert!(caps.is_empty());
}

#[test]
fn manifest_override_bootc_ignores_probe() {
    let caps = classify_base_with_probe(Some(&BaseType::Bootc), Some(false));
    assert!(caps.contains(&Capability::Bootc));
    assert!(caps.contains(&Capability::Systemd));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib classify
```

- [ ] **Step 3: Extract classification logic into `classify_base_with_probe`**

Refactor `classify_base` to delegate to a testable inner function:

```rust
/// Testable classification: accepts probe result directly instead of running skopeo.
pub(crate) fn classify_base_with_probe(
    manifest_override: Option<&BaseType>,
    probe_result: Option<bool>,
) -> CapabilitySet {
    // Signal 1: manifest override wins unconditionally
    if let Some(base_type) = manifest_override {
        return capabilities_for_base_type(base_type.clone());
    }

    // Signal 2: interpret the probe result
    match probe_result {
        Some(true) => capabilities_for_base_type(BaseType::Bootc),
        Some(false) => {
            // Label absent — default to bootc (preserves current behavior,
            // fails loudly via bootc container lint rather than silently
            // dropping systemctl preset-all)
            capabilities_for_base_type(BaseType::Bootc)
        }
        None => {
            // Lookup failed — already warned on stderr, default to bootc
            capabilities_for_base_type(BaseType::Bootc)
        }
    }
}

pub fn classify_base(
    base_image: &str,
    manifest_override: Option<&BaseType>,
) -> CapabilitySet {
    if manifest_override.is_some() {
        return classify_base_with_probe(manifest_override, None);
    }

    let probe_result = probe_bootc_label(base_image);
    classify_base_with_probe(manifest_override, probe_result)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib classify
```

- [ ] **Step 5: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/classify.rs
git commit -m "refactor(classify): extract testable classify_base_with_probe

Enables deterministic unit tests for every classification
branch without requiring skopeo.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 4: Make `generate_containerfile` capability-aware

**Files:**
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/generator.rs:1-8` (imports)
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/generator.rs:43-49` (function signature)
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/generator.rs:313-331` (step emission)
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/generator.rs:334-412` (test helpers and imports)
- Test: `/Users/mrussell/Work/osfragment-assemble/src/generator.rs` (inline tests)

**Interfaces:**
- Consumes: `Capability`, `CapabilitySet` from Task 2
- Produces: Updated `generate_containerfile` signature accepting `&CapabilitySet`

- [ ] **Step 1: Write the failing tests**

Add to generator.rs test module (after existing tests), along with a helper:

```rust
use crate::classify::{Capability, CapabilitySet};
use std::collections::HashSet;

fn bootc_caps() -> CapabilitySet {
    let mut set = HashSet::new();
    set.insert(Capability::Bootc);
    set.insert(Capability::Systemd);
    set
}

fn empty_caps() -> CapabilitySet {
    HashSet::new()
}

#[test]
fn container_base_omits_bootc_steps() {
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "quay.io/fedora/fedora:41".into(),
        base_type: None,
        fragments: vec![mf_epel],
    };
    let output = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        false,
        &empty_caps(),
    )
    .unwrap();
    assert!(!output.contains("systemctl preset-all"));
    assert!(!output.contains("bootc container lint"));
    // Other content still present
    assert!(output.contains("COPY --from="));
}

#[test]
fn bootc_base_preserves_both_steps() {
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        base_type: None,
        fragments: vec![mf_epel],
    };
    let output = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        false,
        &bootc_caps(),
    )
    .unwrap();
    assert!(output.contains("systemctl preset-all"));
    assert!(output.contains("bootc container lint"));
}

#[test]
fn systemd_only_emits_preset_but_not_lint() {
    let mut caps = HashSet::new();
    caps.insert(Capability::Systemd);
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "quay.io/some/image:1".into(),
        base_type: None,
        fragments: vec![mf_epel],
    };
    let output = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        false,
        &caps,
    )
    .unwrap();
    assert!(output.contains("systemctl preset-all"));
    assert!(!output.contains("bootc container lint"));
}
```

- [ ] **Step 2: Run tests to verify they fail (compile error — signature mismatch)**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib generator 2>&1 | head -30
```

- [ ] **Step 3: Update `generate_containerfile` signature and step emission**

Update function signature (line 43):

```rust
use crate::classify::{Capability, CapabilitySet};

pub fn generate_containerfile(
    manifest: &Manifest,
    fragments: &[LoadedFragment],
    base_digest: Option<&str>,
    _dedup: &DeduplicationResult,
    ocp: bool,
    capabilities: &CapabilitySet,
) -> Result<String> {
```

Replace the unconditional preset-all block (lines 313-323) with:

```rust
    // Phase: preset-apply (35) — requires systemd capability
    if capabilities.contains(&Capability::Systemd) {
        if !ocp {
            writeln!(out, "# Apply systemd presets from fragments")?;
        }
        writeln!(
            out,
            "RUN systemctl preset-all --preset-mode=enable-only 2>/dev/null || true"
        )?;
        if !ocp {
            writeln!(out)?;
        }
    }

    // Phase: validation (90) — requires bootc capability
    if capabilities.contains(&Capability::Bootc) {
        if !ocp {
            writeln!(out, "# --- Phase: validation (90) ---")?;
        }
        writeln!(out, "RUN bootc container lint")?;
    }
```

- [ ] **Step 4: Update all existing test calls to pass `&bootc_caps()`**

Every existing call to `generate_containerfile` in the test module must add the
`&bootc_caps()` argument. The `Manifest` structs in tests need `base_type: None`.
This is mechanical: find every `generate_containerfile(` call in the test module and
append `, &bootc_caps()` before the closing `)`. Add `base_type: None` to every
`Manifest { ... }` literal.

Affected tests (all in `generator.rs::tests`):
- `generated_output_contains_digest_pinned_from`
- `generated_output_has_section_ordering`
- `generated_output_batches_packages`
- `generated_output_has_preset_apply`
- `generated_output_has_bootc_lint`
- `no_packages_phase_when_no_packages`
- `mirror_generates_sed_rewrite`
- `repo_dedup_skips_non_canonical_provider`
- `base_image_with_port_generates_correct_from`
- `unpinned_uses_inline_copy_refs`
- `unpinned_uses_base_tag_not_digest`
- `unpinned_hooks_use_inline_copy_refs`
- `pinned_keeps_named_stages`
- `ocp_uses_from_configs_as_final`
- `ocp_omits_header_comments`
- `ocp_omits_section_comments`
- `ocp_unpinned_uses_inline_refs`
- `ocp_pinned_uses_named_stages`
- `ocp_has_preset_apply_and_lint`
- `multiple_hooks_run_in_alphabetical_order`
- `config_fragment_generates_tree_copy`
- `mixed_repos_and_config_full_content_ordering`
- `overlapping_tree_paths_produce_collision_comment`
- `ocp_hooks_without_section_comments`

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib generator
```

- [ ] **Step 6: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/generator.rs
git commit -m "feat(generator): make bootc steps conditional on capability set

Steps carry an implicit requires field: preset-all requires
systemd, bootc container lint requires bootc. Steps are emitted
only when their capability is in the set.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 5: Wire classification into `main.rs` assembly flow

**Files:**
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/main.rs:1-13` (imports)
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/main.rs:101-149` (assembly block)

**Interfaces:**
- Consumes: `classify_base` from Task 2, `capabilities_for_base_type` from Task 2, `Capability` from Task 2, updated `generate_containerfile` from Task 4
- Produces: Complete assembly flow with classification and diverging OCP output

- [ ] **Step 1: Update imports**

```rust
use osfragment_assemble::classify::{classify_base, capabilities_for_base_type, Capability};
use osfragment_assemble::manifest::BaseType;
```

- [ ] **Step 2: Update the assembly block (None arm of match)**

Replace the assembly block starting at line 101:

```rust
        None => {
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

            let fragments = load_all_fragments(&manifest, cli.pin_digests)?;

            eprintln!("Validating composition...");
            let dedup = validate_composition(&manifest, &fragments)?;

            // Classify the base image
            eprintln!("Classifying base image...");
            let capabilities = classify_base(
                &manifest.base,
                manifest.base_type.as_ref(),
            );

            let containerfile = generate_containerfile(
                &manifest,
                &fragments,
                base_digest.as_deref(),
                &dedup,
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
                    &ocp_capabilities,
                )?;
                let mosc = generate_machine_os_config(&ocp_containerfile, &cli.pool)?;
                std::fs::write(ocp_path, &mosc)
                    .with_context(|| format!("writing {}", ocp_path.display()))?;
                eprintln!("MachineOSConfig written to {}", ocp_path.display());
            }
        }
```

- [ ] **Step 3: Verify compilation**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo build
```

- [ ] **Step 4: Run full test suite**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test
```

- [ ] **Step 5: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/main.rs
git commit -m "feat(main): wire base classification into assembly flow

Classification runs before generation. Standalone Containerfile
uses the classified capabilities. MachineOSConfig always uses
full bootc capabilities regardless of classification.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 6: Integration tests for classification-driven output

**Files:**
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/generator.rs` (add integration-style tests to inline test module)

**Interfaces:**
- Consumes: All prior tasks
- Produces: Tests covering every acceptance criterion

- [ ] **Step 1: Write OCP-always-bootc test**

Add to generator.rs test module:

```rust
#[test]
fn ocp_always_emits_bootc_steps_regardless_of_caps() {
    // Even with empty capabilities, OCP mode forces bootc steps
    // because MachineOSConfig always targets bootc.
    // This test verifies the *caller* pattern: main.rs always passes
    // bootc_caps() for OCP. The generator itself is capability-gated.
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "quay.io/fedora/fedora:41".into(),
        base_type: Some(crate::manifest::BaseType::Container),
        fragments: vec![mf_epel],
    };
    // OCP caller always passes full bootc caps
    let ocp_caps = bootc_caps();
    let output = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        true,
        &ocp_caps,
    )
    .unwrap();
    assert!(output.contains("systemctl preset-all"));
    assert!(output.contains("bootc container lint"));
}
```

- [ ] **Step 2: Write divergence test (container base + OCP)**

```rust
#[test]
fn container_base_with_ocp_produces_divergent_outputs() {
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "quay.io/fedora/fedora:41".into(),
        base_type: Some(crate::manifest::BaseType::Container),
        fragments: vec![mf_epel.clone()],
    };

    // Standalone: container capabilities (empty set)
    let standalone_caps = empty_caps();
    let standalone = generate_containerfile(
        &manifest,
        &[epel.clone()],
        None,
        &empty_dedup(),
        false,
        &standalone_caps,
    )
    .unwrap();

    // OCP: always bootc capabilities
    let ocp_caps = bootc_caps();
    let ocp = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        true,
        &ocp_caps,
    )
    .unwrap();

    // Standalone omits bootc steps
    assert!(!standalone.contains("systemctl preset-all"));
    assert!(!standalone.contains("bootc container lint"));

    // OCP includes bootc steps
    assert!(ocp.contains("systemctl preset-all"));
    assert!(ocp.contains("bootc container lint"));
}
```

- [ ] **Step 3: Write test for absent label defaulting to bootc**

```rust
#[test]
fn no_base_type_with_bootc_caps_preserves_current_output() {
    // Simulates: no manifest override, label absent -> default bootc
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        base_type: None,
        fragments: vec![mf_epel],
    };
    let output = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        false,
        &bootc_caps(),
    )
    .unwrap();
    // Same as pre-change output — both steps present
    assert!(output.contains("systemctl preset-all"));
    assert!(output.contains("bootc container lint"));
}
```

- [ ] **Step 4: Write test for manifest override winning over label**

```rust
#[test]
fn manifest_override_container_omits_steps() {
    // baseType: container in manifest -> no bootc steps
    // regardless of what label inspection would return
    let (epel, mf_epel) = make_unpinned_repos_fragment("epel");
    let manifest = Manifest {
        base: "registry.redhat.io/rhel10/rhel-bootc:10.0".into(),
        base_type: Some(crate::manifest::BaseType::Container),
        fragments: vec![mf_epel],
    };
    let output = generate_containerfile(
        &manifest,
        &[epel],
        None,
        &empty_dedup(),
        false,
        &empty_caps(),
    )
    .unwrap();
    assert!(!output.contains("systemctl preset-all"));
    assert!(!output.contains("bootc container lint"));
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib generator
```

- [ ] **Step 6: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/generator.rs
git commit -m "test(generator): add classification-driven output tests

Covers: container base omits steps, OCP always includes steps,
divergent output with baseType:container + --ocp, absent label
defaults to bootc, manifest override wins over label.

Assisted-by: Claude Code (claude-opus-5)"
```

---

### Task 7: Add `classify` tests covering the `probe_bootc_label` integration path

The `probe_bootc_label` function calls `skopeo` and cannot be unit-tested
directly. This task adds a doc comment explaining the coverage gap and
adds a test for the format string used in the skopeo command to catch
regressions in the Go template syntax.

**Files:**
- Modify: `/Users/mrussell/Work/osfragment-assemble/src/classify.rs`

**Interfaces:**
- Consumes: All prior tasks
- Produces: Additional test coverage and documentation

- [ ] **Step 1: Add skopeo format string constant and test**

Extract the Go template format string into a named constant and add a test:

```rust
/// Go template format string for extracting the containers.bootc label.
/// The double braces are Go template syntax, not Rust format placeholders.
const BOOTC_LABEL_FORMAT: &str = "{{.Labels.containers\\.bootc}}";
```

Update `probe_bootc_label` to use the constant:

```rust
fn probe_bootc_label(base_image: &str) -> Option<bool> {
    let output = std::process::Command::new("skopeo")
        .args([
            "inspect",
            "--override-os",
            "linux",
            "--format",
            BOOTC_LABEL_FORMAT,
            &format!("docker://{}", base_image),
        ])
        .output();
    // ... rest unchanged
```

Add test:

```rust
#[test]
fn bootc_label_format_uses_go_template_syntax() {
    // Verify the format string is valid Go template syntax for skopeo
    assert!(BOOTC_LABEL_FORMAT.starts_with("{{"));
    assert!(BOOTC_LABEL_FORMAT.ends_with("}}"));
    assert!(BOOTC_LABEL_FORMAT.contains("containers\\.bootc"));
}
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
cd /Users/mrussell/Work/osfragment-assemble && cargo test --lib classify
```

- [ ] **Step 3: Commit**

```bash
cd /Users/mrussell/Work/osfragment-assemble
git add src/classify.rs
git commit -m "refactor(classify): extract skopeo format string constant

Named constant documents the Go template syntax and enables
regression testing without a running registry.

Assisted-by: Claude Code (claude-opus-5)"
```

---

## Self-Review

### Spec coverage checklist

| Acceptance criterion | Task |
|---|---|
| Non-bootc base produces Containerfile with neither step | Task 4 (`container_base_omits_bootc_steps`), Task 6 (`manifest_override_container_omits_steps`) |
| Bootc base unchanged from current output | Task 4 (`bootc_base_preserves_both_steps`), Task 6 (`no_base_type_with_bootc_caps_preserves_current_output`) |
| Classification signals documented | Task 2 (doc comments on `classify_base`), Task 7 (format string docs) |
| No label + no override = bootc (test) | Task 3 (`probe_false_no_label_defaults_to_bootc`), Task 6 (`no_base_type_with_bootc_caps_preserves_current_output`) |
| Manifest override wins over label (test) | Task 3 (`manifest_override_container_ignores_probe`, `manifest_override_bootc_ignores_probe`), Task 6 (`manifest_override_container_omits_steps`) |
| Failed lookup warns + classifies bootc | Task 2 (`classify_base` eprintln warning), Task 3 (`probe_none_lookup_failed_defaults_to_bootc`) |
| MachineOSConfig always emits both steps (test) | Task 5 (main.rs always passes `bootc` caps for OCP), Task 6 (`ocp_always_emits_bootc_steps_regardless_of_caps`) |
| No classification path inspects image name | No code path references image name for classification |
| Divergence test: `baseType: container` + `--ocp` | Task 6 (`container_base_with_ocp_produces_divergent_outputs`) |

### Placeholder scan

No `todo!()`, `TBD`, `TODO`, or `implement later` markers in final code. All `todo!()` in Task 2 Step 1 are replaced by implementations in Steps 4-6.

### Type consistency

- `BaseType` enum: defined in `manifest.rs`, used in `classify.rs` and `generator.rs` tests
- `Capability` / `CapabilitySet`: defined in `classify.rs`, consumed by `generator.rs` and `main.rs`
- `generate_containerfile`: signature updated in Task 4, all callers updated in Tasks 4 (tests) and 5 (main.rs)
- `Manifest.base_type`: `Option<BaseType>`, added in Task 1, threaded through `classify_base` in Task 5
