use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use osfragment_assemble::generator::generate_containerfile;
use osfragment_assemble::inspect::run_inspect;
use osfragment_assemble::list::run_list;
use osfragment_assemble::loader::{
    load_all_fragments, load_registry_fragment_metadata_only, resolve_digest,
    should_keep_fragment_digests,
};
use osfragment_assemble::manifest::parse_manifest;
use osfragment_assemble::mount::MountMaterialization;
use osfragment_assemble::ocp::generate_machine_os_config;
use osfragment_assemble::self_contained::{
    check_mount_materialization, check_target_dir_safe, create_archive, write_output,
};
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

    /// Write build-mount material into the --self-contained build context.
    /// Mount material is credential material more often than not, so the
    /// context and its archive become a durable copy of it: the mount
    /// subtrees are written owner-only and a .gitignore keeps the context
    /// out of git while it holds them.
    #[arg(long, requires = "self_contained")]
    materialize_mounts: bool,

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
            let manifest_data = parse_manifest(&content, &manifest.display().to_string())?;

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
            let manifest = parse_manifest(&content, &cli.manifest.display().to_string())?;

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

            if let Some(dir) = &cli.self_contained {
                check_mount_materialization(dir, &fragments, cli.materialize_mounts)?;

                let containerfile = generate_containerfile(
                    &manifest,
                    &fragments,
                    base_digest.as_deref(),
                    false,
                    true,
                )?;

                // Archive after the swap, not from the staging tree. Archiving
                // pre-swap is possible in principle (tar takes the
                // in-archive name independently of the source path, so the
                // staging directory's temporary name need not appear in the
                // tarball), but not reachable from here: write_output does not
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
                write_output(
                    dir,
                    &cli.manifest,
                    &containerfile,
                    &fragments,
                    MountMaterialization::from_flag(cli.materialize_mounts),
                )?;
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
                )?;

                std::fs::write(&cli.output, &containerfile)
                    .with_context(|| format!("writing {}", cli.output.display()))?;

                eprintln!(
                    "Containerfile written to {} ({} fragments)",
                    cli.output.display(),
                    fragments.len()
                );

                // OCP MachineOSConfig generation
                if let Some(ocp_path) = &cli.ocp {
                    let ocp_containerfile = generate_containerfile(
                        &manifest,
                        &fragments,
                        base_digest.as_deref(),
                        true,
                        false,
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
