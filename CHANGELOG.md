# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`--self-contained <dir>`** - Materializes fragment tree/hooks payload into a local build context whose `Containerfile` sits alongside the payload, then packages the result as a sibling `.tar.gz`. The output directory carries a `.osfragment-assemble` sentinel file that marks it as tool-generated and safe to regenerate. The emitted Containerfile references no registry image except the base. Mutually exclusive with `--ocp` and `--output`.

### Removed

- **`phase` in `fragment.toml`** - The field and its `com.github.marrusl.osfragment.phase` annotation are gone, along with the `repos`-phase content restriction that forbade hooks and non-repo tree paths. It never decided placement: where a file lands has always been determined by its path, so a `config` fragment's repo definitions were hoisted ahead of the package install just like a `repos` fragment's. What it did do was sort fragments by phase weight before emission, which silently overrode manifest order and could decide which fragment won a path collision. Emission is now pure manifest order, matching the documented contract that manifest order is user intent. Stale `phase` keys are ignored rather than rejected, in the TOML and in the annotations, so previously published fragments keep resolving without a rebuild; rebuild them to drop the dead key.

### Changed

- **Repo deduplication** - The tool no longer prints `skipping duplicate repo files from '<name>'`. It never skipped anything: every provider emitted its own COPY and the last one won. Fragments providing the same repo ID with identical content now pass silently, and the collision is reported where it always was, in the generated Containerfile's header comment. Conflicting content for the same repo ID still fails the build.
- **Namespace** - Manifest `apiVersion` is now `osfragment/v1alpha1` (was `bootc.io/v1alpha1`) and OCI annotation keys are now `com.github.marrusl.osfragment.<key>` (was `io.bootc.fragment.<key>`), moving both to a namespace this project controls. No compatibility path is provided: the old annotation keys are not read, so previously published fragments must be rebuilt and republished with the new keys or they fall back to layer extraction for metadata. Update `apiVersion` in existing manifests; the tool does not validate its value, so stale manifests parse without complaint.
- **Hooks** - Renamed fragment directory from `scripts/` to `hooks/`; all files under `fragment/hooks/` are collected as executables regardless of extension, allowing Python, Perl, or any interpreter-based hook
