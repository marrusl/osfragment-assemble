# Codebase Layout

Directory structure and module organization for osfragment-assemble. Answers
one question: where does what live, and what is each part responsible for?

The crate is a single library plus a thin binary. There is no workspace and
there are no sub-crates. Every module is one file directly under `src/`.

## Repository structure

```
/
├── src/              # The whole crate: one file per module
├── tests/            # Integration tests (cli.rs)
├── examples/         # Example fragments and manifests
│   ├── fragments/    # 10 buildable fragment sources
│   └── manifests/    # 6 composition manifests
├── docs/             # User-facing docs, plus POC-era specs and plans
├── process-docs/     # Specs, plans, and skill files (internal)
└── tmp/              # Gitignored scratch, never committed
```

`README.md` is the authoritative user documentation. `docs/fragment-format.md`
and `docs/rationales.md` sit beside it, and `docs/specs/` and `docs/plans/`
hold POC records that are historical rather than current. See
[documentation-authority.md](documentation-authority.md) for which file wins
when they disagree.

## Source modules

| File | Responsibility |
|------|----------------|
| `src/main.rs` | `clap` CLI definition, subcommand dispatch, and the two helpers `load_all_fragments` and `should_keep_fragment_digests` |
| `src/lib.rs` | Module declarations only, no logic |
| `src/manifest.rs` | Parses the composition YAML into `Manifest` and `ManifestFragment`. Owns `BaseType` and `FragmentSource` |
| `src/fragment.rs` | The `fragment.toml` data model and parser. Owns the `FragmentName` newtype, `REPO_PREFIXES`, and `is_repo_path` |
| `src/loader.rs` | Pulls fragment images via `skopeo`, reads metadata from OCI annotations or by walking layers, validates tar entries, and materializes fragment payload to disk. Produces `LoadedFragment` |
| `src/classify.rs` | Turns the base image's declared `baseType` into a `CapabilitySet` of `Capability::Bootc` and `Capability::Systemd`. Declared-or-default, no network |
| `src/validate.rs` | Composition checks across loaded fragments: duplicate names, declared conflicts, repo file collisions |
| `src/generator.rs` | `generate_containerfile`: emits the Containerfile. Also `split_image_ref` |
| `src/self_contained.rs` | `--self-contained` output mode: sentinel-guarded target checks, staged and atomic materialization into a build context, sibling tarball packaging |
| `src/ocp.rs` | Wraps a generated Containerfile in a MachineOSConfig YAML for OpenShift on-cluster layering |
| `src/inspect.rs` | Rendering for the `inspect` subcommand |
| `src/list.rs` | Rendering for the `list` subcommand |

**Dependency direction:** `main.rs` drives everything. `generator.rs` and
`self_contained.rs` consume `loader.rs`, which consumes `fragment.rs`.
`validate.rs` consumes `loader.rs`. `classify.rs` consumes only `manifest.rs`.
Nothing in the library depends on `main.rs`.

**Where the network is touched:** only `src/loader.rs`, and only through
`std::process::Command::new("skopeo")`. Three call sites: digest resolution,
`inspect --raw` for the annotation fast path, and `copy` into a temporary OCI
layout. Nothing else in the crate reaches a registry. For proving changes to
that code against a real registry, see
[registry-verification.md](registry-verification.md).

## CLI surface

Defined in `src/main.rs`. Running with no subcommand is the assembly path.

| Invocation | Effect |
|------------|--------|
| `osfragment-assemble` | Reads `--manifest`, writes a Containerfile to `--output` |
| `osfragment-assemble inspect <target>` | Examines a fragment image reference or a local fragment directory (`src/inspect.rs`) |
| `osfragment-assemble list [--manifest <path>]` | Prints the manifest's fragments in manifest order (`src/list.rs`) |

Flags on the assembly path:

| Flag | Default | Notes |
|------|---------|-------|
| `--manifest <PATH>` | `osfragment-assemble.yaml` | |
| `--output <PATH>` | `Containerfile` | |
| `--pin-digests` | off | With `--self-contained`, affects the base image only |
| `--ocp [FILE]` | `machineosbuild.yaml` when passed bare | Emits a MachineOSConfig alongside the Containerfile |
| `--self-contained <DIR>` | off | Conflicts with `--ocp` and `--output` |
| `--pool <NAME>` | `worker` | Meaningful only with `--ocp` |

## Emitted output

**Containerfile section order** (`generate_containerfile` in
`src/generator.rs`) is fixed, and each section is preceded by a banner
comment: fragment stages, base, repo files, packages, config files, hooks,
systemd preset application, then bootc validation. The last two are emitted
only when the capability set from `src/classify.rs` includes them. Fragments
are emitted in manifest order throughout. What the emitted directives actually
guarantee is covered in
[containerfile-layer-semantics.md](containerfile-layer-semantics.md).

**Self-contained build context** (`write_output` in `src/self_contained.rs`)
is a directory containing `Containerfile`, `manifest.yaml`, `fragments/`, and
the `.osfragment-assemble` sentinel file, plus a sibling `<dir>.tar.gz` written
by `create_archive`.

**MachineOSConfig** (`src/ocp.rs`) wraps the Containerfile with a size cap on
the embedded content. The environment it targets is described in
[ocl-build-environment.md](ocl-build-environment.md).

## Tests

Unit tests are inline `#[cfg(test)]` modules at the bottom of each source
file. `tests/cli.rs` is the only integration test file. See
[testing-surfaces.md](testing-surfaces.md) for what each surface can and
cannot exercise, and for what is known to be unpinnable.

## Examples

`examples/fragments/` holds 10 fragment sources: `awscli-zip`,
`cis-hardening`, `epel`, `grafana`, `hashicorp`, `nginx`, `node-exporter`,
`nvidia-driver-run`, `postgresql`, `tailscale`. Each has a `fragment.toml` and
a `Containerfile.fragment`; the ones that need them also carry `tree/` and
`hooks/`. `examples/manifests/` holds 6 composition manifests, of which
`demo.yaml` composes one fragment of each supported kind.

Two of these fragments carry a large vendor blob fetched by their own script
rather than committed. See
[blob-carrying-fragments.md](blob-carrying-fragments.md).

## Process docs

```
process-docs/
├── skills/           # Non-obvious patterns and gotchas (this file)
├── specs/
│   ├── proposed/
│   └── implemented/
└── plans/            # Implementation plans, flat, no proposed/implemented split
```

`process-docs/skills/index.md` is the entry point. A skill file that is not
listed there is invisible to future sessions.

## Gotchas

- **Two locations hold specs and plans.** New work goes in `process-docs/`.
  `docs/specs/` and `docs/plans/` are POC records kept for history.
- **`src/generator.rs` is mostly tests.** The emitting code ends around line
  385; everything after that is one inline test module. The same pattern
  applies to `src/loader.rs` and `src/self_contained.rs`, where the test
  module is the larger part of the file.
- **`main.rs` is not a pure dispatcher.** `load_all_fragments` and
  `should_keep_fragment_digests` live there, outside the library, so they are
  reachable only from the binary and its own inline tests.
- **Fragment-supplied values reach filesystem paths.** Where each one is
  checked, and what to accept as a parameter type when you add a path join, is
  in [fragment-input-invariants.md](fragment-input-invariants.md).
- **No CI configuration exists in this repo.** `cargo clippy -- -D
  clippy::all` and `cargo fmt --check` are run locally and gate commits.
- **`tmp/` is gitignored.** So is `/target`, a `/Containerfile` generated at
  the repo root, `/demo-context/` and its tarball, and the vendor blobs under
  `examples/fragments/nvidia-driver-run/hooks/` and
  `examples/fragments/awscli-zip/hooks/`.
