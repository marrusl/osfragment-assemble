use std::collections::HashSet;

use crate::manifest::BaseType;

/// Capabilities that a base image may provide.
///
/// `generate_containerfile` emits an optional step only when the base's
/// capability set contains what that step needs: `systemctl preset-all`
/// needs `Systemd`, `RUN bootc container lint` needs `Bootc`. The check is
/// a `contains` call at each step's emission site; there is no table of
/// steps and no `requires` field.
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

/// One line describing a `baseType` override that changes what gets emitted,
/// or `None` when there is nothing to report.
///
/// Only `container` qualifies: it drops the Systemd and Bootc capabilities, so
/// the preset and lint steps disappear from the output. A declared `bootc` is
/// the default, so announcing it would describe a change that did not happen.
pub fn base_type_override_note(manifest_override: Option<&BaseType>) -> Option<&'static str> {
    match manifest_override {
        Some(BaseType::Container) => {
            Some("Base type: container (manifest override); systemd preset and lint steps omitted")
        }
        Some(BaseType::Bootc) | None => None,
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

    #[test]
    fn container_override_is_worth_reporting() {
        let note = base_type_override_note(Some(&BaseType::Container)).expect("expected a note");
        assert!(note.contains("container"));
    }

    #[test]
    fn declared_bootc_matches_the_default_so_there_is_nothing_to_report() {
        assert!(base_type_override_note(Some(&BaseType::Bootc)).is_none());
    }

    #[test]
    fn absent_base_type_reports_nothing() {
        assert!(base_type_override_note(None).is_none());
    }
}
