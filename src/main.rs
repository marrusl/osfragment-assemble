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
use osfragment_assemble::validate::validate_composition;

#[derive(Parser)]
#[command(
    name = "osfragment-assemble",
    version,
    about = "Generate Containerfiles from composable fragment images for bootc-compatible OS images",
    long_about = "Generate Containerfiles from composable fragment images for bootc-compatible OS images.\n\n\
        Run without a subcommand to read a manifest and generate a Containerfile.\n\
        Use 'inspect' to examine a fragment or 'list' to show manifest contents."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the manifest file
    #[arg(long, default_value = "osfragment-assemble.yaml")]
    manifest: PathBuf,

    /// Output path for the generated Containerfile
    #[arg(long, default_value = "Containerfile")]
    output: PathBuf,

    /// Pin all image references to resolved digests for reproducibility
    #[arg(long)]
    pin_digests: bool,

    /// Generate a MachineOSConfig YAML for OpenShift on-cluster builds
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "machineosbuild.yaml")]
    ocp: Option<PathBuf>,

    /// MachineConfigPool name for --ocp output (only meaningful with --ocp)
    #[arg(long, default_value = "worker")]
    pool: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect a fragment image or directory
    Inspect {
        /// Fragment image reference or local directory path
        target: String,
    },
    /// List fragments from the manifest in phase-sorted order
    List {
        /// Path to the manifest file
        #[arg(long, default_value = "osfragment-assemble.yaml")]
        manifest: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Inspect { target }) => {
            run_inspect(&target)?;
        }
        Some(Commands::List { manifest }) => {
            let content = std::fs::read_to_string(&manifest)
                .with_context(|| format!("reading manifest {}", manifest.display()))?;
            let manifest_data = parse_manifest(&content)?;

            let mut fragments = Vec::new();
            let total = manifest_data.fragments.len();
            for (idx, mf) in manifest_data.fragments.iter().enumerate() {
                let source = mf.resolve_source()?;
                let osfragment_assemble::manifest::FragmentSource::Registry { ref image_ref } =
                    source;
                eprintln!(
                    "Loading fragment metadata {}/{}: {}...",
                    idx + 1,
                    total,
                    image_ref
                );
                let mut loaded = load_registry_fragment_metadata_only(image_ref)?;
                loaded.manifest_index = idx;
                fragments.push(loaded);
            }
            fragments.sort_by(|a, b| {
                a.fragment
                    .phase
                    .weight()
                    .cmp(&b.fragment.phase.weight())
                    .then(a.manifest_index.cmp(&b.manifest_index))
            });
            run_list(&manifest_data, &fragments)?;
        }
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
    }

    Ok(())
}

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
        eprintln!(
            "  {} ({})",
            loaded.fragment.name, loaded.fragment.description
        );
        loaded.manifest_index = idx;
        fragments.push(loaded);
    }

    // Sort by phase weight; within same weight, preserve manifest order
    fragments.sort_by(|a, b| {
        a.fragment
            .phase
            .weight()
            .cmp(&b.fragment.phase.weight())
            .then(a.manifest_index.cmp(&b.manifest_index))
    });
    Ok(fragments)
}
