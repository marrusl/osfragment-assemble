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

/// Go template for extracting the containers.bootc label via skopeo inspect.
/// Uses the index function because the dotted key is not a valid Go field path.
const BOOTC_LABEL_FORMAT: &str = "{{index .Labels \"containers.bootc\"}}";

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
    if manifest_override.is_some() {
        return classify_base_with_probe(manifest_override, None);
    }

    let probe_result = probe_bootc_label(base_image);
    classify_base_with_probe(manifest_override, probe_result)
}

/// Query the `containers.bootc` label from the base image config via skopeo.
/// Returns `Some(true)` if the label exists and is non-empty,
/// `Some(false)` if the label is absent or empty,
/// `None` if the lookup failed.
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

    #[test]
    fn failed_probe_warns_on_stderr() {
        // classify_base with a nonexistent image triggers the warning path.
        // We can't easily capture stderr in a unit test, but we can verify
        // the function returns bootc capabilities and does not panic/error.
        let caps = classify_base("nonexistent.invalid/no-such-image:1", None);
        assert!(caps.contains(&Capability::Bootc));
        assert!(caps.contains(&Capability::Systemd));
    }

    #[test]
    fn bootc_label_format_uses_go_template_syntax() {
        assert!(BOOTC_LABEL_FORMAT.starts_with("{{"));
        assert!(BOOTC_LABEL_FORMAT.ends_with("}}"));
        assert!(BOOTC_LABEL_FORMAT.contains("containers.bootc"));
        assert!(BOOTC_LABEL_FORMAT.contains("index"));
    }
}
