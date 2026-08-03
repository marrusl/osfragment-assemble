# Hooks Entrypoint Contract

**Status:** Implemented
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
- **`entrypoint` is a symlink.** Cannot occur on the registry path:
  `validate_tar_entry` (`loader.rs:83-88`) rejects any symlink or hardlink
  anywhere in a fragment layer, before this rule is evaluated, so such a
  fragment gets the tar-level error instead. On a local directory `std::fs`
  follows the link, so a symlinked `entrypoint` resolving to an executable
  regular file is accepted. The divergence is deliberate: the registry path
  refuses links for unrelated safety reasons and this rule does not relax that.

The "regular file" wording has no silent hole on the registry path for the same
reason: the two non-regular types that could hide something runnable, symlinks
and hardlinks, are rejected outright before the rule applies.

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
  currently discards — `extract_tree_paths_from_bytes` keeps only paths. This is
  a single-pass change, not a second pass: the loop at `loader.rs:141-142`
  already reads `entry.header()`, so the mode is one more field off a header it
  is holding.
- **`run_inspect` on a local directory.** Fires here, against the filesystem
  mode (`fs::metadata().permissions().mode()`). **The local hook scan must be
  made recursive first.** It is currently a single non-recursive `read_dir` over
  `hooks/` (`inspect.rs:20-27`), so it cannot see nested files, while the rule
  counts hook files at any depth. The sibling `tree/` scan in the same function
  is already recursive (`collect_display_paths` → `collect_display_recursive`,
  `inspect.rs:91-107`); follow that pattern. Without this the two validation
  sites disagree: a fragment whose `hooks/` holds only `lib/helper.sh` requires
  an entrypoint by the rule, and the local scan would see zero files and pass
  it. That matters more than it looks, because local directories never reach
  assembly (`resolve_source` bails on `dir:`), which makes `inspect` an author's
  *only* pre-publish check rather than a lesser one.
- **`load_registry_fragment_metadata_only`** (annotation fast path). Cannot fire.
  It reads OCI annotations and never pulls a layer, so it cannot see `hooks/` at
  all; it already returns `hook_paths: vec![]`. This is accepted rather than
  fixed: the path exists to avoid layer pulls, and forcing one would remove its
  only reason to exist. Its sole consumer is `list`, which never emits a build.
  No caveat line is added to `list` output — it would print on every invocation
  for a condition `list` cannot detect, which is a permanent disclaimer rather
  than information.

The practical consequence, stated precisely: `list` **may** succeed against a
non-conforming fragment. The fast path only fires when the required annotations
are present; a fragment lacking them falls back to the full load and *is*
validated, so the same defect produces two different `list` outcomes depending
on an unrelated optimization. Assembly always full-loads, so nothing reaches a
Containerfile unvalidated.

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
sort, join, or filter: the loader no longer needs an ordered list, and emission
asks only whether the fragment has hooks at all. Whether `hook_paths` changes
type is left to the plan (see Open) — `inspect` still displays the hook file
list, and that listing is worth keeping.

Registry mode, for a fragment whose reference is `<ref>`:

```dockerfile
RUN --mount=type=bind,from=<ref>,source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z \
    /frag-hooks/entrypoint
```

Self-contained mode, for a fragment named `<name>`:

```dockerfile
RUN --mount=type=bind,source=fragments/<name>/hooks,target=/frag-hooks,z \
    /frag-hooks/entrypoint
```

**OCP mode emits the registry form unchanged.** The `if !ocp` guards around hook
emission wrap only the section comments (`generator.rs:334-352`), so OCP is
covered by construction rather than by a third branch. Called out because this
interaction has been wrong before: the OCP-specific assertion that
`bind-propagation=rshared,z` is present (`generator.rs:1478`) exists for that
reason.

All modes keep the current two-line shape and every mount form is unchanged,
including `bind-propagation=rshared` on the registry form and its deliberate
absence on the self-contained form. The only change is the command: a fixed
`/frag-hooks/entrypoint` in place of the chained invocations.

The second line is now the literal `    /frag-hooks/entrypoint` for every
fragment in every mode. The mount line still varies, in three shapes rather than
two: in registry mode by whichever reference `copy_from_source`
(`generator.rs:33-40`) yields — a named stage `frag-<name>` when any fragment in
the set is digest-pinned, the inline image reference otherwise — and in
self-contained mode by the fragment name.

Two code comments die with the logic beneath them:
`// Collect all executable files in hooks/ directory` at `loader.rs:404` and
`inspect.rs:17`. Both sit above filters that test only a path prefix and check
no mode, and both should be deleted rather than carried onto the entrypoint
check, where they would be wrong in a new way.

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

Because all six are already `0755`, the exec-bit rule adds no migration burden
at all: it costs nothing to adopt and only ever fires on a genuine authoring
mistake.

## Tests

Registry-path validation tests can use the existing `create_test_tarball_with_modes`
fixture and its `RawEntry { path, data, mode, entry_type }` struct
(`loader.rs:539-569`), which already takes arbitrary modes. No new test
infrastructure is needed.

- Validation, missing: a fragment with `hooks/other.sh` and no `entrypoint` is
  rejected, and the error names the fragment and `hooks/entrypoint`.
- Validation, not executable: `hooks/entrypoint` at mode `0644` is rejected with
  the mode message.
- Valid shapes accepted: zero hook files; `entrypoint` alone; `entrypoint`
  alongside other files; `entrypoint` alongside a subdirectory.
- Subdirectory contents are never invoked, and a nested `hooks/lib/entrypoint`
  does not satisfy the rule.
- **Validation on a local directory:** `inspect` on a fragment directory with
  `hooks/other.sh` and no `entrypoint` is rejected with the same message, and a
  directory whose `hooks/` holds only `lib/helper.sh` is likewise rejected. The
  second case is what pins the recursive scan; without it the local and registry
  sites can diverge with a green suite.
- Emission, registry mode: exactly one `RUN --mount` per hook-carrying fragment,
  ending in `/frag-hooks/entrypoint`, with no other `/frag-hooks/` path present.
- Emission, self-contained mode: same, with the context-relative mount form.
- Emission, OCP mode: same registry mount form, `bind-propagation=rshared,z`
  still present, one invocation.
- Invocation is fixed, not merely uniform: over a manifest with **at least two**
  hook-carrying fragments, at a **fixed pinning mode**, every emitted invocation
  line equals the literal `    /frag-hooks/entrypoint`. Asserting the lines equal
  each other is not sufficient — one fragment satisfies it trivially, and so does
  a uniformly wrong invocation. Run it once pinned and once unpinned, since the
  mount line's shape differs between them.
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

- Whether `hook_paths: Vec<PathBuf>` becomes `has_hooks: bool` now or keeps its
  shape until a consumer needs more. Emission needs only the boolean, but
  `inspect` still displays the hook file list, so the list has a live consumer
  either way.

Settled, recorded here so the plan does not reopen them: the mode check is a
single-pass change (`loader.rs:141-142` already reads the header); the mode-
carrying test fixture already exists (`loader.rs:539-569`); and the exec-bit
requirement stays. Dropping the exec check would leave the spec detecting a
missing entrypoint at load while deferring an unusable one to a build log — two
defects of the same class, discoverable at the same moment, at the same cost.
`0o111` is the correct predicate rather than a convenient one: root's
`CAP_DAC_OVERRIDE` grants execute only when at least one execute bit is set, so
the mask matches exactly what is runnable at build time.
