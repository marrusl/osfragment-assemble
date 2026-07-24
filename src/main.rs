use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use bootc_assemble::generator::generate_containerfile;
use bootc_assemble::inspect::run_inspect;
use bootc_assemble::list::run_list;
use bootc_assemble::loader::{
    load_local_fragment, load_registry_fragment, load_registry_fragment_metadata_only,
    prebuild_local_fragment, resolve_digest, resolve_local_digest,
};
use bootc_assemble::manifest::{parse_manifest, FragmentSource};
use bootc_assemble::validate::validate_composition;

#[derive(Parser)]
#[command(name = "bootc-assemble", version, about = "Composable image definitions for bootc and RHCOS")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the manifest file
    #[arg(long, default_value = "bootc-assemble.yaml")]
    manifest: PathBuf,

    /// Output path for the generated Containerfile
    #[arg(long, default_value = "Containerfile")]
    output: PathBuf,

    /// After generating, run podman build
    #[arg(long)]
    build: bool,

    /// Treat all fragment image values as local directory paths
    #[arg(long)]
    local: bool,
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

        /// Treat all fragment image values as local directory paths
        #[arg(long)]
        local: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Inspect { target }) => {
            run_inspect(&target)?;
        }
        Some(Commands::List { manifest, local }) => {
            let content = std::fs::read_to_string(&manifest)
                .with_context(|| format!("reading manifest {}", manifest.display()))?;
            let manifest_data = parse_manifest(&content)?;

            // List is metadata-only — read fragment.toml directly for
            // dir: sources (no prebuild), use metadata-only for registry.
            let mut fragments = Vec::new();
            for (idx, mf) in manifest_data.fragments.iter().enumerate() {
                let source = if local {
                    FragmentSource::Directory {
                        path: PathBuf::from(
                            mf.image.strip_prefix("dir:").unwrap_or(&mf.image),
                        ),
                    }
                } else {
                    mf.resolve_source()
                };
                let mut loaded = match &source {
                    FragmentSource::Directory { .. } => load_local_fragment(&source)?,
                    FragmentSource::Registry { image_ref } => {
                        load_registry_fragment_metadata_only(image_ref)?
                    }
                };
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

            let (fragments, temp_images) = load_all_fragments(&manifest, cli.local)?;
            let dedup = validate_composition(&manifest, &fragments)?;

            // Always resolve base image digest — the base is a registry
            // image even when fragments are local directories.
            // Hard-fail if unreachable: an unpinned base violates the
            // digest contract.
            let base_digest = Some(resolve_digest(&manifest.base)?);

            let containerfile =
                generate_containerfile(&manifest, &fragments, base_digest.as_deref(), &dedup)?;

            std::fs::write(&cli.output, &containerfile)
                .with_context(|| format!("writing {}", cli.output.display()))?;

            eprintln!("Containerfile written to {}", cli.output.display());

            if cli.build {
                let status = std::process::Command::new("podman")
                    .args(["build", "-f", &cli.output.to_string_lossy(), "."])
                    .status()
                    .context("failed to run podman build")?;
                if !status.success() {
                    anyhow::bail!("podman build failed");
                }
            }

            // Clean up prebuilt temp images
            for tag in &temp_images {
                let _ = std::process::Command::new("podman")
                    .args(["rmi", tag])
                    .output();
            }
        }
    }

    Ok(())
}

fn load_all_fragments(
    manifest: &bootc_assemble::manifest::Manifest,
    local: bool,
) -> Result<(Vec<bootc_assemble::loader::LoadedFragment>, Vec<String>)> {
    let mut fragments = Vec::new();
    let mut temp_images: Vec<String> = Vec::new();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = if local {
            FragmentSource::Directory {
                path: PathBuf::from(mf.image.strip_prefix("dir:").unwrap_or(&mf.image)),
            }
        } else {
            mf.resolve_source()
        };

        let mut loaded = match &source {
            FragmentSource::Directory { path } => {
                // Prebuild to temp local image, resolve its digest,
                // then load metadata from the local directory
                let frag_toml = std::fs::read_to_string(path.join("fragment.toml"))?;
                let frag_meta = bootc_assemble::fragment::parse_fragment_toml(&frag_toml)?;
                let tag = prebuild_local_fragment(path, &frag_meta.name)?;
                temp_images.push(tag.clone());
                // Resolve digest from local podman storage (not skopeo —
                // the temp image is not in a registry)
                let local_digest = resolve_local_digest(&tag).with_context(|| {
                    format!(
                        "resolving digest for prebuilt fragment '{}'",
                        frag_meta.name
                    )
                })?;
                let pinned_ref = format!("{}@{}", tag, local_digest);
                let mut l = load_local_fragment(&source)?;
                l.source = FragmentSource::Registry {
                    image_ref: pinned_ref,
                };
                l.resolved_digest = Some(local_digest);
                l
            }
            FragmentSource::Registry { image_ref } => load_registry_fragment(image_ref)?,
        };
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
    Ok((fragments, temp_images))
}
