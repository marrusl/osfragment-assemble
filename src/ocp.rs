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
            "OCP Containerfile exceeds {} character limit ({} chars) \
             — reduce fragments or packages to fit within MachineOSConfig API limits",
            OCP_CONTAINERFILE_LIMIT,
            containerfile.len()
        );
    }

    let mut out = String::new();
    writeln!(
        out,
        "apiVersion: machineconfiguration.openshift.io/v1alpha1"
    )?;
    writeln!(out, "kind: MachineOSConfig")?;
    writeln!(out, "metadata:")?;
    writeln!(out, "  name: {pool}")?;
    writeln!(out, "spec:")?;
    writeln!(out, "  machineConfigPool:")?;
    writeln!(out, "    name: {pool}")?;
    writeln!(out, "  buildInputs:")?;
    writeln!(out, "    containerFile:")?;
    writeln!(out, "      - containerfileArch: noarch")?;
    writeln!(out, "        content: |")?;

    for line in containerfile.lines() {
        if line.is_empty() {
            writeln!(out)?;
        } else {
            writeln!(out, "          {line}")?;
        }
    }

    writeln!(out, "    imageBuilder:")?;
    writeln!(out, "      imageBuilderType: PodImageBuilder")?;
    writeln!(out, "    renderedImagePushSecret:")?;
    writeln!(
        out,
        "      name: REPLACE_WITH_SECRET_NAME  # e.g., builder-dockercfg-xxxxx"
    )?;
    writeln!(out, "    renderedImagePushspec: image-registry.openshift-image-registry.svc:5000/openshift-machine-config-operator/os-images:latest")?;
    writeln!(out, "  buildOutputs:")?;
    writeln!(out, "    currentImagePullSecret:")?;
    writeln!(
        out,
        "      name: REPLACE_WITH_SECRET_NAME  # must match renderedImagePushSecret for internal registry"
    )?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_yaml_structure() {
        let containerfile = "FROM configs AS final\nCOPY --from=quay.io/test/epel:10 /fragment/tree/ /\nRUN bootc container lint\n";
        let output = generate_machine_os_config(containerfile, "worker").unwrap();
        assert!(output.contains("apiVersion: machineconfiguration.openshift.io/v1alpha1"));
        assert!(output.contains("kind: MachineOSConfig"));
        assert!(output.contains("  name: worker"));
        assert!(output.contains("    name: worker"));
        assert!(output.contains("        content: |"));
        assert!(output.contains("          FROM configs AS final"));
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
        // Content lines are indented by 10 spaces
        assert!(output.contains("          FROM configs AS final"));
        assert!(output.contains("          RUN echo hello"));
    }
}
