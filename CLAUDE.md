# osfragment-assemble

Composable image definitions for bootc-compatible OS images.

## Orientation

Read `process-docs/skills/index.md` first for skills files covering non-obvious patterns and correctness requirements.

Key modules:
- `src/manifest.rs` — YAML manifest parsing and data structures
- `src/loader.rs` — pulls fragment images, extracts metadata and tree/hooks content
- `src/generator.rs` — emits Containerfile from loaded fragments
- `src/classify.rs` — determines base image type (bootc vs. plain container)
- `src/ocp.rs` — MachineOSConfig YAML generation for OpenShift on-cluster layering
- `src/inspect.rs`, `src/list.rs`, `src/validate.rs` — CLI subcommands
- `src/self_contained.rs` — `--self-contained` output mode: sentinel-guarded target checks, staged/atomic materialization into a build context, sibling tar.gz packaging

## Key Conventions

- **Clippy clean:** `cargo clippy -- -D clippy::all` with zero warnings. Non-negotiable.
- **Format:** `cargo fmt --check` must pass.
- **Commit format:** `type(scope): description` in imperative mood. Attribution: `Assisted-by: Claude Code (<model>)`.
- **Attribution:** LLM-assisted commits include `Assisted-by: <tool> (<model>)`. No team member names (this is a public repo).
- **Specs and plans** go in `process-docs/specs/` and `process-docs/plans/`.
- **Skill file maintenance:** If your work reveals a non-obvious pattern, workaround, or correctness requirement, capture it in a skill file (new or existing) and update `process-docs/skills/index.md`.
