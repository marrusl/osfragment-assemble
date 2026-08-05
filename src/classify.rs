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

/// Classify the base image and return its capability set.
///
/// Classification is declared-or-default: the manifest's `baseType` decides,
/// and absent it the base is treated as `Bootc`.
///
/// This used to probe the base's `containers.bootc` label over the network
/// first. The probe was removed because it could not change the answer: a
/// present label, an absent label, and a failed lookup all classified as
/// `Bootc`, so the only path to any other classification was — and remains —
/// the manifest override, which never consulted the probe. Generation
/// therefore needs no registry access for the base.
pub fn classify_base(_base_image: &str, manifest_override: Option<&BaseType>) -> CapabilitySet {
    match manifest_override {
        Some(base_type) => capabilities_for_base_type(base_type.clone()),
        None => capabilities_for_base_type(BaseType::Bootc),
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
    fn manifest_override_bootc_yields_bootc_caps() {
        let caps = classify_base("nonexistent.invalid/image:1", Some(&BaseType::Bootc));
        assert!(caps.contains(&Capability::Bootc));
        assert!(caps.contains(&Capability::Systemd));
    }

    #[test]
    fn manifest_override_container_yields_empty_set() {
        let caps = classify_base("nonexistent.invalid/image:1", Some(&BaseType::Container));
        assert!(caps.is_empty());
    }

    #[test]
    fn no_override_defaults_to_bootc_without_touching_the_registry() {
        // The image is deliberately unresolvable: classification must not
        // depend on reaching it, which is the whole point of dropping the probe.
        let caps = classify_base("nonexistent.invalid/no-such-image:1", None);
        assert!(caps.contains(&Capability::Bootc));
        assert!(caps.contains(&Capability::Systemd));
    }
}
