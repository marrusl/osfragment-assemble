# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`nvidia-driver-run` example fragment** - Installs the NVIDIA driver from the vendor's `.run` self-extracting installer, compiling the kernel modules for the kernel of the image being built rather than the build host's. The first example whose `hooks/entrypoint` does real work, the first to carry a large binary payload as hook material, and the first to use `conflicts.fragments`. The ~350 MB installer is bind-mounted for one `RUN` and contributes zero bytes to the built image; the build toolchain is installed and removed inside that same `RUN` rather than declared as `packages.required`, so it leaves nothing recoverable in an earlier layer. The installer is never committed: `fetch-run-installer.sh` downloads it against a recorded sha256 and extracts the NVIDIA `LICENSE` that ships with it, and the blob is listed in `.gitignore`. Accompanied by `examples/manifests/nvidia-driver-run.yaml`.

- **`--self-contained <dir>`** - Materializes fragment tree/hooks payload into a local build context whose `Containerfile` sits alongside the payload, then packages the result as a sibling `.tar.gz`. The output directory carries a `.osfragment-assemble` sentinel file that marks it as tool-generated and safe to regenerate. The emitted Containerfile references no registry image except the base. Mutually exclusive with `--ocp` and `--output`.

### Removed

- **`phase` in `fragment.toml`** - The field and its `com.github.marrusl.osfragment.phase` annotation are gone, along with the `repos`-phase content restriction that forbade hooks and non-repo tree paths. It never decided placement: where a file lands has always been determined by its path, so a `config` fragment's repo definitions were hoisted ahead of the package install just like a `repos` fragment's. What it did do was sort fragments by phase weight before emission, which silently overrode manifest order and could decide which fragment won a path collision. Emission is now pure manifest order, matching the documented contract that manifest order is user intent. Stale `phase` keys are ignored rather than rejected, in the TOML and in the annotations, so previously published fragments keep resolving without a rebuild; rebuild them to drop the dead key.

### Changed

- **Repo deduplication** - The tool no longer prints `skipping duplicate repo files from '<name>'`. It never skipped anything: every provider emitted its own COPY and the last one won. Fragments providing the same repo ID with identical content now pass silently, and the collision is reported where it always was, in the generated Containerfile's header comment. Conflicting content for the same repo ID still fails the build.
- **Namespace** - Manifest `apiVersion` is now `osfragment/v1alpha1` (was `bootc.io/v1alpha1`) and OCI annotation keys are now `com.github.marrusl.osfragment.<key>` (was `io.bootc.fragment.<key>`), moving both to a namespace this project controls. No compatibility path is provided: the old annotation keys are not read, so previously published fragments must be rebuilt and republished with the new keys or they fall back to layer extraction for metadata. Update `apiVersion` in existing manifests; the tool does not validate its value, so stale manifests parse without complaint.
- **Hooks entrypoint (breaking)** - If a fragment's `hooks/` contains any file, it must contain an executable regular file named `entrypoint`, and that file is the only thing the tool runs. Everything else under `hooks/`, at any depth, is support material the entrypoint can reach at `/frag-hooks/` but the tool never invokes. Alphabetical chaining of every file in `hooks/` is gone, with no fallback to discovery: a fragment carrying hooks and no executable `hooks/entrypoint` now fails to load, at assembly and at `inspect` alike. Unlike the `phase` removal, which retired a key the tool stopped reading, this changes what the tool runs — **every published fragment carrying hooks must be rebuilt**, or it stops loading entirely. Migration is usually a rename: `git mv hooks/configure.sh hooks/entrypoint`. Fragments needing several steps get an entrypoint that invokes the others, in whatever order and with whatever arguments they actually need.
- **Hooks** - Renamed fragment directory from `scripts/` to `hooks/`; the tool infers nothing from a hook file's extension, allowing Python, Perl, or any interpreter-based hook
