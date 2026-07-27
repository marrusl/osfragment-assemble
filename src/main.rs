use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use bootc_assemble::generator::generate_containerfile;
use bootc_assemble::inspect::run_inspect;
use bootc_assemble::list::run_list;
use bootc_assemble::loader::{
    load_registry_fragment, load_registry_fragment_metadata_only, resolve_digest,
};
use bootc_assemble::manifest::parse_manifest;
use bootc_assemble::validate::validate_composition;

#[derive(Parser)]
#[command(
    name = "bootc-assemble",
    version,
    about = "Composable image definitions for bootc and RHCOS"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the manifest file
    #[arg(long, default_value = "bootc-assemble.yaml")]
    manifest: PathBuf,

    /// Output path for the generated Containerfile
    #[arg(long, default_value = "Containerfile")]
    output: PathBuf,
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
        #[arg(long, default_value = "bootc-assemble.yaml")]
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
                let bootc_assemble::manifest::FragmentSource::Registry { ref image_ref } = source;
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

            eprintln!("Resolving base image digest...");
            let base_digest = Some(resolve_digest(&manifest.base)?);

            let fragments = load_all_fragments(&manifest)?;

            eprintln!("Validating composition...");
            let dedup = validate_composition(&manifest, &fragments)?;

            let containerfile =
                generate_containerfile(&manifest, &fragments, base_digest.as_deref(), &dedup)?;

            std::fs::write(&cli.output, &containerfile)
                .with_context(|| format!("writing {}", cli.output.display()))?;

            eprintln!(
                "Containerfile written to {} ({} fragments)",
                cli.output.display(),
                fragments.len()
            );
        }
    }

    Ok(())
}

fn load_all_fragments(
    manifest: &bootc_assemble::manifest::Manifest,
) -> Result<Vec<bootc_assemble::loader::LoadedFragment>> {
    let mut fragments = Vec::new();
    let total = manifest.fragments.len();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = mf.resolve_source()?;
        let bootc_assemble::manifest::FragmentSource::Registry { ref image_ref } = source;
        eprintln!("Loading fragment {}/{}: {}...", idx + 1, total, image_ref);
        let mut loaded = load_registry_fragment(image_ref)?;
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
