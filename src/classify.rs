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
