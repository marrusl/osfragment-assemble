# Self-Contained Mode

**Status:** Proposed
**Date:** 2026-07-29

Adds one output mode: `--self-contained <dir>` materializes fragment contents
into a local build context next to the generated Containerfile, then packages
the result as a tarball. The output builds with no registry access except the
base image, commits cleanly to git, and hands off as a single file.

**Why.** The generated Containerfile normally references fragment images, so
every build depends on a registry and on whatever those references resolve to
at build time. Some workflows want the opposite trade: pull once at generation
time, then carry everything in the build context. A gitops repo gets
reviewable diffs of vendored content. An isolated pipeline gets one artifact
to move. And the eject story becomes complete: a user who stops using the
tool keeps a fully self-contained context, not a Containerfile that still
needs fragment images to resolve. This also closes most of the fragment side
of the reproducibility gap documented in `docs/rationales.md`; what is in the
tree is pinned by the commit that holds it.

## Surface

- `--self-contained <dir>` on the existing command.
- Mutually exclusive with `--ocp`; passing both is an error. On-cluster
  builds have no user-controlled build context, so the mode cannot apply
  there.
- Everything upstream of emission is unchanged: manifest parsing, fragment
  pulls, composition validation, and repo deduplication run exactly as in
  the default mode.

## Output

```
<dir>/
  Containerfile
  manifest.yaml              copy of the input manifest, so a recipient of
                             the handoff can regenerate without hunting for it
  fragments/
    <name>/
      tree/                  materialized payload
      hooks/                 materialized hooks (when present)
<dir>.tar.gz                 sibling archive of the same content
```

- Materialization reuses the loader's existing pull and extract path; no new
  fetch machinery.
- The archive contains a single top-level directory named after the basename
  of `<dir>`, so extraction is predictable.
- The tree serves gitops diffing; the tarball serves handoff. Users delete
  whichever they do not need. No flag suppresses the archive yet; if size
  becomes a problem in practice, that flag is the relief valve.

## Emission changes

Two substitutions relative to default mode, both into context-relative forms:

- Payload: `COPY --from=<stage or image> /fragment/tree/ /` becomes
  `COPY fragments/<name>/tree/ /`.
- Hooks: the bind mount drops its `from=`; `source=fragments/<name>/hooks`
  resolves against the build context. Hook bytes still never enter an image
  layer; the build-inputs property of default mode carries over unchanged.
  Mount options stay identical to default mode for consistency (the
  implementation plan verifies whether any option is meaningless for a
  context source and may drop it there).

`FROM <base>` is unchanged and is the only registry reference in the emitted
Containerfile. Builds in isolated environments mirror the base image exactly
as they do today.

## Update model: regenerate, never mutate

The output is a pure function of the manifest and registry state at
generation time. To update, re-run the command; the tool deletes and
recreates `<dir>` in full. There is no lockfile, no incremental update, and
no update subcommand. A user who commits the tree gets the lock (the commit)
and the review mechanism (`git diff` after regeneration) from git itself; the
tool ships no diff machinery.

## Errors

- `--self-contained` with `--ocp`: error stating the modes are exclusive.
- `<dir>` may be absent, empty, or tool-generated (containing both the
  `Containerfile` and `fragments/`). Anything else is refused rather than
  deleted. This protects against pointing the flag at a directory the user
  owns.
- Any registry failure during materialization fails the whole run. No
  partial tree is ever left behind.

## Acceptance

- Golden-file test on the emitted Containerfile in self-contained mode.
- Integration test materializing from local fixture fragments, asserting the
  archive contents match the tree byte for byte.
- A test asserting the emitted Containerfile contains no registry reference
  other than the base image. This is the mode's defining invariant.
- A test that an existing non-tool-generated `<dir>` is refused.
- A test that `--self-contained` plus `--ocp` errors.

## Non-goals

- No OCP interaction.
- No provenance or signature recording. Users verify fragments at generation
  time; afterward, trust belongs to the tree and whatever holds it.
- No lockfile, no partial update, no in-place mutation.
- No archive-suppression flag yet.

## Open for the implementation plan

- Whether `--pin-digests` should affect generation-time pulls in this mode.
  Nothing remote remains in the emitted Containerfile to pin, but the pulls
  that build the tree and the base reference may still warrant it; check
  current generator and loader behavior before deciding.
- Short single-dash flag aliases are tracked separately and out of scope
  here.
