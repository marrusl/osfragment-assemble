use anyhow::{bail, Result};
use std::fmt::Write;

/// Maximum characters allowed in the MachineOSConfig containerFile content.
/// The MCO API rejects payloads exceeding this limit.
const OCP_CONTAINERFILE_LIMIT: usize = 4096;

/// Generate a MachineOSConfig YAML manifest that wraps an OCP Containerfile
/// for use with the Machine Config Operator's on-cluster build system.
pub fn generate_machine_os_config(containerfile: &str, pool: &str) -> Result<String> {
    if containerfile.len() > OCP_CONTAINERFILE_LIMIT {
        bail!(
            "OCP Containerfile exceeds {} character limit ({} chars): \
             reduce fragments or packages to fit within MachineOSConfig API limits",
            OCP_CONTAINERFILE_LIMIT,
            containerfile.len()
        );
    }

    let mut out = String::new();
    writeln!(out, "apiVersion: machineconfiguration.openshift.io/v1")?;
    writeln!(out, "kind: MachineOSConfig")?;
    writeln!(out, "metadata:")?;
    writeln!(out, "  name: {pool}")?;
    writeln!(out, "spec:")?;
    writeln!(out, "  machineConfigPool:")?;
    writeln!(out, "    name: {pool}")?;
    writeln!(out, "  containerFile:")?;
    writeln!(out, "    - containerfileArch: NoArch")?;
    writeln!(out, "      content: |")?;

    for line in containerfile.lines() {
        if line.is_empty() {
            writeln!(out)?;
        } else {
            writeln!(out, "        {line}")?;
        }
    }

    writeln!(out, "  imageBuilder:")?;
    writeln!(out, "    imageBuilderType: Job")?;
    writeln!(out, "  renderedImagePushSecret:")?;
    writeln!(
        out,
        "    name: REPLACE_WITH_SECRET_NAME  # e.g., builder-dockercfg-xxxxx"
    )?;
    writeln!(out, "  renderedImagePushSpec: image-registry.openshift-image-registry.svc:5000/openshift-machine-config-operator/os-images:latest")?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_v1_yaml_structure() {
        let containerfile = "FROM configs AS final\nCOPY --from=quay.io/test/epel:10 /fragment/tree/ /\nRUN bootc container lint\n";
        let output = generate_machine_os_config(containerfile, "worker").unwrap();
        // v1 API version
        assert!(output.contains("apiVersion: machineconfiguration.openshift.io/v1"));
        assert!(!output.contains("v1alpha1"));
        assert!(output.contains("kind: MachineOSConfig"));

        // name-matching rule: metadata.name == spec.machineConfigPool.name
        assert!(output.contains("  name: worker"));
        assert!(output.contains("    name: worker"));

        // v1 flat fields, not buildInputs/buildOutputs
        assert!(output.contains("  containerFile:"));
        assert!(!output.contains("buildInputs"));
        assert!(!output.contains("buildOutputs"));

        // Content structure
        assert!(output.contains("    - containerfileArch: NoArch"));
        assert!(output.contains("      content: |"));
        assert!(output.contains("        FROM configs AS final"));

        // Job builder type
        assert!(output.contains("  imageBuilder:"));
        assert!(output.contains("    imageBuilderType: Job"));
        assert!(!output.contains("PodImageBuilder"));

        // v1 field names with correct casing
        assert!(output.contains("  renderedImagePushSpec:"));
        assert!(output.contains("  renderedImagePushSecret:"));

        // baseImagePullSecret not emitted
        assert!(!output.contains("baseImagePullSecret"));
        // currentImagePullSecret removed
        assert!(!output.contains("currentImagePullSecret"));
    }

    #[test]
    fn uses_custom_pool_name() {
        let containerfile = "FROM configs AS final\nRUN bootc container lint\n";
        let output = generate_machine_os_config(containerfile, "infra").unwrap();
        // metadata.name and machineConfigPool.name both use pool
        let name_count = output.matches("name: infra").count();
        assert_eq!(name_count, 2);
    }

    #[test]
    fn rejects_oversized_containerfile() {
        let containerfile = "x".repeat(OCP_CONTAINERFILE_LIMIT + 1);
        let result = generate_machine_os_config(&containerfile, "worker");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("4096"));
    }

    #[test]
    fn indents_containerfile_content() {
        let containerfile = "FROM configs AS final\nRUN echo hello\n";
        let output = generate_machine_os_config(containerfile, "worker").unwrap();
        // Content lines are indented by 8 spaces (v1 structure)
        assert!(output.contains("        FROM configs AS final"));
        assert!(output.contains("        RUN echo hello"));
    }

    #[test]
    fn emits_noarch_in_pascal_case() {
        let containerfile = "FROM configs AS final\n";
        let output = generate_machine_os_config(containerfile, "worker").unwrap();
        // PascalCase NoArch, not lowercase noarch
        assert!(output.contains("containerfileArch: NoArch"));
        assert!(!output.contains("containerfileArch: noarch"));
    }

    #[test]
    fn emits_rendered_image_push_secret_at_spec_level() {
        let containerfile = "FROM configs AS final\n";
        let output = generate_machine_os_config(containerfile, "worker").unwrap();
        // v1 location: spec.renderedImagePushSecret
        assert!(output.contains("  renderedImagePushSecret:"));
        assert!(output.contains("    name: REPLACE_WITH_SECRET_NAME"));
    }
}
