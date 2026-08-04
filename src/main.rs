use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use osfragment_assemble::classify::{capabilities_for_base_type, classify_base};
use osfragment_assemble::generator::generate_containerfile;
use osfragment_assemble::inspect::run_inspect;
use osfragment_assemble::list::run_list;
use osfragment_assemble::loader::{
    load_registry_fragment, load_registry_fragment_metadata_only, resolve_digest,
};
use osfragment_assemble::manifest::{parse_manifest, BaseType};
use osfragment_assemble::ocp::generate_machine_os_config;
use osfragment_assemble::self_contained::{check_target_dir_safe, create_archive, write_output};
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

    /// Pin image references to resolved digests for reproducibility (with
    /// --self-contained, affects the base image only)
    #[arg(long)]
    pin_digests: bool,

    /// Generate a MachineOSConfig YAML for OpenShift on-cluster builds
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "machineosbuild.yaml")]
    ocp: Option<PathBuf>,

    /// Materialize fragment contents into a local build context and
    /// package it as a tarball, so the emitted Containerfile needs no
    /// registry access at build time except for the base image. Mutually
    /// exclusive with --ocp and --output: this mode's Containerfile lives
    /// only at <dir>/Containerfile.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["ocp", "output"])]
    self_contained: Option<PathBuf>,

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
    /// List fragments from the manifest in manifest order
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
            run_list(&manifest_data, &fragments)?;
        }
        None => {
            if let Some(dir) = &cli.self_contained {
                check_target_dir_safe(dir)?;
            }

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

            let keep_digests =
                should_keep_fragment_digests(cli.pin_digests, cli.self_contained.as_deref());
            let fragments = load_all_fragments(&manifest, keep_digests)?;

            eprintln!("Validating composition...");
            validate_composition(&manifest, &fragments)?;

            // Classify the base image
            eprintln!("Classifying base image...");
            let capabilities = classify_base(&manifest.base, manifest.base_type.as_ref());

            if let Some(dir) = &cli.self_contained {
                let containerfile = generate_containerfile(
                    &manifest,
                    &fragments,
                    base_digest.as_deref(),
                    false,
                    true,
                    &capabilities,
                )?;

                // Archive after the swap, not from the staging tree. Archiving
                // pre-swap is possible in principle — tar takes the
                // in-archive name independently of the source path, so the
                // staging directory's temporary name need not appear in the
                // tarball — but not reachable from here: write_output does not
                // expose its staging path, and keep() consumes the TempDir.
                // Getting at it would mean changing that interface, which is
                // out of scope, not impossible. Post-swap, <dir> holds exactly
                // the tree that was staged, satisfying the spec's "built from
                // the same staged tree".
                //
                // The cost of this order is the window below: the tree is
                // already in place while archiving can still fail, and
                // create_archive deliberately leaves a pre-existing
                // <dir>.tar.gz untouched rather than truncating it, so a
                // failure here can leave an older archive beside a newer
                // tree. Recoverable, but it must never be silent, hence the
                // context.
                write_output(dir, &cli.manifest, &containerfile, &fragments)?;
                let archive_path = create_archive(dir).with_context(|| {
                    format!(
                        "packaging {} as an archive; the build context directory itself is \
                         complete and usable, but its sibling .tar.gz was not written, so any \
                         archive still sitting there is from an earlier run and no longer \
                         matches {}. Re-run to retry packaging, or build directly from the \
                         directory and delete the stale archive",
                        dir.display(),
                        dir.display()
                    )
                })?;

                eprintln!(
                    "Self-contained context written to {} ({} fragments)",
                    dir.display(),
                    fragments.len()
                );
                eprintln!("Archive written to {}", archive_path.display());
            } else {
                let containerfile = generate_containerfile(
                    &manifest,
                    &fragments,
                    base_digest.as_deref(),
                    false,
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
                        true,
                        false,
                        &ocp_capabilities,
                    )?;
                    let mosc = generate_machine_os_config(&ocp_containerfile, &cli.pool)?;
                    std::fs::write(ocp_path, &mosc)
                        .with_context(|| format!("writing {}", ocp_path.display()))?;
                    eprintln!("MachineOSConfig written to {}", ocp_path.display());
                }
            }
        }
    }

    Ok(())
}

/// `keep_digests`: whether to leave each fragment's digest-pinned
/// `FragmentSource`/`resolved_digest` in place. See
/// `should_keep_fragment_digests` for why this isn't simply `pin_digests`.
fn load_all_fragments(
    manifest: &osfragment_assemble::manifest::Manifest,
    keep_digests: bool,
) -> Result<Vec<osfragment_assemble::loader::LoadedFragment>> {
    let mut fragments = Vec::new();
    let total = manifest.fragments.len();

    for (idx, mf) in manifest.fragments.iter().enumerate() {
        let source = mf.resolve_source()?;
        let osfragment_assemble::manifest::FragmentSource::Registry { ref image_ref } = source;
        eprintln!("Loading fragment {}/{}: {}...", idx + 1, total, image_ref);
        let mut loaded = load_registry_fragment(image_ref)?;
        if !keep_digests {
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

    // No reordering: emission follows manifest order, which is user intent.
    Ok(fragments)
}

/// Whether fragment digests (and the digest-pinned `FragmentSource`) should
/// survive `load_all_fragments` for use downstream.
///
/// `--pin-digests` keeps them for default mode's named-stage emission and
/// digest comments, as before. `--self-contained` also needs them kept,
/// independently of `--pin-digests`: materialization must pull exactly the
/// digest that was validated, even though the emitted Containerfile never
/// exposes that digest (see generator.rs's self-contained suppression).
fn should_keep_fragment_digests(pin_digests: bool, self_contained: Option<&Path>) -> bool {
    pin_digests || self_contained.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_fragment_digests_cases() {
        assert!(!should_keep_fragment_digests(false, None));
        assert!(should_keep_fragment_digests(false, Some(Path::new("out"))));
        assert!(should_keep_fragment_digests(true, None));
        assert!(should_keep_fragment_digests(true, Some(Path::new("out"))));
    }
}
