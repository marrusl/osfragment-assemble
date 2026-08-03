# Hooks Entrypoint Contract

**Status:** Proposed
**Date:** 2026-08-03

If a fragment's `hooks/` directory contains any file, it must contain an
executable regular file named `entrypoint`, and that file is the only thing
osfragment-assemble runs. Everything else under `hooks/` is support material,
available to the entrypoint at the mount path but never invoked by the tool.
Failing the rule is a validation error at fragment load.

**Why.** Today every regular file under `hooks/` is executed, in alphabetical
order, with no arguments. That breaks the moment a fragment ships an executable
that is not a hook: a vendor installer such as NVIDIA's `.run` needs
`--no-questions --ui=none` and, run bare, either hangs waiting for input or
half-configures the image. Support binaries, helper libraries, and payload
executables are all mistaken for hooks. Ordering by filename is the only
sequencing control available, and there is no place to put arguments,
conditionals, or cleanup. A single named entrypoint hands all of that to a real
program, in a language that has control flow, and reduces what the tool must
infer to nothing.

No fallback to automatic discovery is provided. A fallback would preserve
exactly the trap being removed: a fragment whose entrypoint is missing or
misnamed would silently revert to running whatever looks runnable. Inferring
intent from directory contents is the same move refused for merge semantics and
deleted with the `phase` field.

## The rule

Let *hook files* be the regular files under `fragment/hooks/` at any depth.
Directory entries are not hook files.

- **No `hooks/` directory, or zero hook files.** Valid. No hook step is emitted.
- **One or more hook files.** `hooks/entrypoint` must exist, be a regular file,
  and have at least one execute bit set (`0o111`). Otherwise: validation error.
- **`entrypoint` alone.** Valid. It runs.
- **`entrypoint` plus other files.** Valid. Only `entrypoint` runs; the rest are
  present at `/frag-hooks/` for it to use.
- **Subdirectories.** Allowed, at any depth, as support material. Files inside
  them are never invoked, including an `entrypoint` in a subdirectory — only
  `hooks/entrypoint` counts.
- **`entrypoint` present but not executable.** Validation error. Treated the
  same as missing; the message names the mode problem.
- **`entrypoint` exists as a directory.** Validation error, same as missing.

The tool passes no arguments and sets no environment beyond what the build
already provides. The entrypoint runs as root, once, after packages are
installed, with the fragment's `hooks/` bind-mounted at `/frag-hooks`.

## Errors

One error, raised at fragment load, naming the fragment and the fix.

Missing entirely:

```
fragment 'nvidia-driver': hooks/ contains files but no executable
hooks/entrypoint; the entrypoint is the single file osfragment-assemble runs.
Rename the script to hooks/entrypoint, or add one that invokes the others.
```

Present but not executable:

```
fragment 'nvidia-driver': hooks/entrypoint is not executable; the entrypoint is
the single file osfragment-assemble runs. Set the execute bit (chmod +x) before
building the fragment image.
```

Both are `bail!` from the loader, consistent with the repo's existing
fragment-load errors, and both are actionable without reading this spec.

## Where validation fires

Three loading paths, and the check belongs to two of them:

- **`load_registry_fragment`** (full load; pulls layers). Fires here. The tar
  entry for `fragment/hooks/entrypoint` carries the mode, which the loader
  currently reads and discards — `extract_tree_paths_from_bytes` keeps only
  paths. Surfacing the mode for this one entry is new work, not a rewiring of
  something existing.
- **`run_inspect` on a local directory.** Fires here, against the filesystem
  mode. Same rule, same messages.
- **`load_registry_fragment_metadata_only`** (annotation fast path). Cannot fire.
  It reads OCI annotations and never pulls a layer, so it cannot see `hooks/` at
  all; it already returns `hook_paths: vec![]`. This is accepted rather than
  fixed: the path exists to avoid layer pulls, and forcing one would remove its
  only reason to exist. Its sole consumer is `list`, which never emits a build.

The practical consequence is worth stating plainly: `list` will succeed against
a non-conforming fragment, and assembly will then fail on it. Assembly always
uses the full load, so nothing reaches a Containerfile unvalidated.

**Stale published fragments.** A fragment published before this contract, whose
`hooks/` holds `configure.sh` and no `entrypoint`, fails at the first load that
materializes its hooks — loudly, with the message above. There is no
compatibility path and none is wanted; this is the same posture as every other
schema change here. It differs from the `phase` removal in one way that matters
to publishers: `phase` removed a key the tool stopped *reading*, so old
fragments kept working, while this changes what the tool *runs*, so every
hook-carrying fragment must be rebuilt.

## Emission

The scan-and-chain logic is deleted, not branched. There is no list of hooks to
sort, join, or filter, and `LoadedFragment.hook_paths` collapses to a single
question: does this fragment have hooks at all.

Registry mode, for a fragment whose image reference is `<image>`:

```dockerfile
RUN --mount=type=bind,from=<image>,source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z \
    /frag-hooks/entrypoint
```

Self-contained mode, for a fragment named `<name>`:

```dockerfile
RUN --mount=type=bind,source=fragments/<name>/hooks,target=/frag-hooks,z \
    /frag-hooks/entrypoint
```

Both keep the current two-line shape and both mount forms are unchanged,
including `bind-propagation=rshared` on the registry form and its deliberate
absence on the self-contained form. The only change is the command: a fixed
`/frag-hooks/entrypoint` in place of the chained invocations.

The second line is now byte-identical for every fragment in every mode. The
mount line still varies, by `from=<image>` in registry mode and by the fragment
name in self-contained mode.

## Example migration

Six of the eight shipped fragments carry hooks, each with exactly one script,
`configure.sh`, already mode `0755`:

| Fragment | Hooks today | Migration |
|---|---|---|
| cis-hardening | `configure.sh` | rename to `entrypoint` |
| grafana | `configure.sh` | rename to `entrypoint` |
| hashicorp | `configure.sh` | rename to `entrypoint` |
| nginx | `configure.sh` | rename to `entrypoint` |
| node-exporter | `configure.sh` | rename to `entrypoint` |
| tailscale | `configure.sh` | rename to `entrypoint` |
| epel | none | none |
| postgresql | none | none |

Every case is a `git mv`. No fragment needs a wrapper, and no script content
changes. All eight are then rebuilt and repushed to `quay.io/marrusl2/fragments/`.

## Tests

- Validation, missing: a fragment with `hooks/other.sh` and no `entrypoint` is
  rejected, and the error names the fragment and `hooks/entrypoint`.
- Validation, not executable: `hooks/entrypoint` at mode `0644` is rejected with
  the mode message.
- Valid shapes accepted: zero hook files; `entrypoint` alone; `entrypoint`
  alongside other files; `entrypoint` alongside a subdirectory.
- Subdirectory contents are never invoked, and a nested `hooks/lib/entrypoint`
  does not satisfy the rule.
- Emission, registry mode: exactly one `RUN --mount` per hook-carrying fragment,
  ending in `/frag-hooks/entrypoint`, with no other `/frag-hooks/` path present.
- Emission, self-contained mode: same, with the context-relative mount form.
- Invocation is fragment-independent: across a multi-fragment manifest, every
  emitted invocation line is byte-identical, and the mount lines differ only in
  the fragment reference.
- Negative control: an old-style fragment carrying `01-setup.sh` and
  `02-config.sh` and no `entrypoint` is rejected rather than chained. This is
  the regression that would reintroduce auto-discovery.

## CHANGELOG

Breaking, under `### Changed`: the tool runs `hooks/entrypoint` and nothing
else; a fragment with hooks and no executable `hooks/entrypoint` now fails to
load. Unlike the `phase` removal, **every published fragment carrying hooks must
be rebuilt** — the old layout does not merely lose an optimization, it stops
loading.

## Non-goals

- **Consumer argument override.** Already covered by the derived-fragment
  pattern: build a fragment `FROM` the original and replace the one file. No new
  mechanism.
- **Hook metadata of any kind.** No arguments, ordering hints, conditionals, or
  declared interpreters in `fragment.toml` or in annotations. The entrypoint is
  a program; that is where such logic goes.
- **Multiple entrypoints or lifecycle phases.** One file, one invocation.
- **Interpreter provisioning.** Unchanged: the fragment author remains
  responsible for the interpreter existing in the image at build time.

## Open for the implementation plan

- Whether the mode check reads the tar header during the existing single pass in
  `load_registry_fragment` or in a narrow second pass for one path.
- Whether `hook_paths: Vec<PathBuf>` becomes `has_hooks: bool` now or keeps its
  shape until a consumer needs more. `inspect` still displays the hook file
  list, which is worth keeping.
