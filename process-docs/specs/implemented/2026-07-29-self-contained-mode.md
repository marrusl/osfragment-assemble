# Self-Contained Mode

**Status:** Implemented
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
- Mutually exclusive with `--output <path>`; passing both is an error. This
  mode's Containerfile lives only at `<dir>/Containerfile`; `--output`'s
  default value does not trigger the conflict, only an explicit `--output`
  alongside `--self-contained` does.
- Everything upstream of emission is unchanged: manifest parsing, fragment
  pulls, composition validation, and repo deduplication run exactly as in
  the default mode.

## Output

```
<dir>/
  .osfragment-assemble         sentinel: proves this directory is
                               tool-owned, see Errors
  Containerfile
  manifest.yaml                copy of the input manifest, always under this
                               name regardless of the input path (see below)
  fragments/
    <name>/
      tree/                    materialized payload (omitted when the
                               fragment has no tree/ content, e.g. a
                               hooks-only fragment)
      hooks/                   materialized hooks (omitted when absent)
<dir>.tar.gz                   sibling archive of the same content, including
                               the sentinel
```

- Materialization reuses the loader's existing pull and layer-extraction
  machinery; the additional step this mode adds is writing the extracted
  content to `<dir>` instead of collecting it into an in-memory path list.
- The archive contains a single top-level directory named after the basename
  of `<dir>`, so extraction is predictable. It is built from the same staged
  tree the sentinel and everything else lands in, so it always includes the
  sentinel.
- The tree serves gitops diffing; the tarball serves handoff. Users delete
  whichever they do not need. No flag suppresses the archive yet; if size
  becomes a problem in practice, that flag is the relief valve.
- `manifest.yaml` is informational: it records the inputs used at generation
  time, so a recipient of the handoff can see (or regenerate from) them
  without hunting for the original file. It is not a guarantee of
  reproducibility. If the source manifest names fragments by tag rather than
  digest, re-running the tool against the copied manifest later can resolve
  different bytes than the tree currently holds; the tree itself, not the
  manifest copy, is the record of what was actually pulled.

## Emission changes

Every `COPY` or `RUN --mount` instruction whose source is a fragment's
materialized content, in every phase, becomes a context-relative form. This
is a general substitution, not a single example: it covers the generic
payload `COPY`, both repo-phase `COPY` lines (`yum.repos.d`, `rpm-gpg`), and
the hook bind mount alike, because all of them are fed by the same
per-fragment resolution the default-mode generator already does. A
composition built entirely from repos-phase fragments has no generic payload
`COPY` at all, so the rule has to name the repo-phase lines explicitly rather
than leave them to be inferred from one example.

- Payload: `COPY --from=<stage or image> /fragment/tree/ /` becomes
  `COPY fragments/<name>/tree/ /`.
- Repo phase: `COPY --from=<stage or image> /fragment/tree/etc/yum.repos.d/
  /etc/yum.repos.d/` becomes `COPY fragments/<name>/tree/etc/yum.repos.d/
  /etc/yum.repos.d/`, and the analogous `rpm-gpg` line substitutes the same
  way.
- Hooks: the bind mount drops its `from=` entirely; `source=fragments/<name>/hooks`
  resolves against the build context instead. The exact emitted instruction
  is:
  ```
  RUN --mount=type=bind,source=fragments/<name>/hooks,target=/frag-hooks,z \
  ```
  This drops `bind-propagation=rshared` relative to default mode's
  `from=<image>` form. That option governs how mount events propagate
  between mount namespaces, which only matters for a live, host-tied mount
  source (default mode's case: an image or build stage). A build-context
  source is a static copy of files already present when the build starts,
  with no submounts to propagate, so the option is inert there. `z` (SELinux
  relabel) is unrelated to propagation and still applies under SELinux
  enforcement, so it stays. Hook bytes still never enter an image layer; the
  build-inputs property of default mode carries over unchanged.
- Fragment `FROM <ref> AS frag-<name>` named stages never appear. Default
  mode emits these only when digests are pinned (`use_named_stages`); this
  mode suppresses them unconditionally, independent of `--pin-digests`,
  because the COPY/mount substitution above replaces the named-stage form
  the same way it replaces the plain-image form. The header's `# Resolved
  digests:` comment block follows the same rule: it never lists a fragment,
  regardless of `--pin-digests`, since the point of the substitution is that
  no fragment registry reference appears anywhere in the output, comments
  included.

`FROM <base>` is unchanged and is the only registry reference anywhere in the
emitted Containerfile, including comments. It is also the only thing
`--pin-digests` still affects in this mode: fragment images are always
resolved and pulled by digest internally regardless of that flag (materialization
must pull exactly what composition validated, not a tag that may have moved
since), but that digest is never exposed in the output; the base image's
`FROM` line is pinned to a digest only when `--pin-digests` is actually
passed, exactly as in default mode today. Builds in isolated environments
mirror the base image exactly as they do today.

## Update model: regenerate, never mutate

The output is a pure function of the manifest and registry state at
generation time. There is no lockfile, no incremental update, and no update
subcommand. A user who commits the tree gets the lock (the commit) and the
review mechanism (`git diff` after regeneration) from git itself; the tool
ships no diff machinery.

To update, re-run the command. The tool materializes the new output into a
staging directory beside `<dir>`, and only after every fragment succeeds does
it atomically replace the old output: the existing `<dir>` (if any) is
removed and the staged directory is renamed into its place. `<dir>.tar.gz` is
rebuilt from the same staged tree on every run and unconditionally
overwritten; it carries no user-added content by construction (it is a pure
repackaging of `<dir>`, never edited directly), so it needs no ownership
check analogous to `<dir>`'s. Regeneration is therefore idempotent for the materialized tree: running the
command twice against unchanged manifest and registry state produces a
byte-identical `<dir>` both times, and the second run's target-directory
check passes against the first run's own output because the sentinel (see
Errors) is part of what gets staged and swapped in. `<dir>.tar.gz` is not
covered by this guarantee: it is a repackaging of `<dir>`, and per-entry tar
metadata (e.g. mtimes) may differ between runs even when the tree itself
does not. Full archive determinism is a tracked follow-up, not a promise
this spec makes.

If materialization fails partway through (registry error, disk full, a
fragment that fails to extract), the run exits nonzero and neither `<dir>`
nor `<dir>.tar.gz` is touched: the staging directory that was being built is
discarded, and the previously-generated output, if any, is exactly as it was
before the run started. There is no window in which the user has neither the
old output nor the new one.

## Errors

- `--self-contained` with `--ocp`: error stating the modes are exclusive.
- `--self-contained` with `--output`: error stating the modes are exclusive.
- `<dir>` may be absent, empty, or tool-generated. Anything else is refused
  rather than deleted, protecting a directory the user owns for their own
  reasons.

  Tool-generated is defined by a sentinel file, `.osfragment-assemble`,
  written into every self-contained output directory, containing the tool
  name and version. Its **presence**, not its exact contents, is what the
  check relies on: a directory is tool-generated if it contains the sentinel
  and no entries outside the set the tool itself writes (`Containerfile`,
  `manifest.yaml`, `fragments/`, the sentinel). Presence of `Containerfile`
  and `fragments/` alone is not sufficient; a directory a user built for
  their own purposes that happens to contain both must not be silently
  deleted just because it pattern-matches. The sentinel is a regular file
  inside `<dir>`, so it is part of what gets committed to git and packaged
  into the archive; a directory checked out from a repo that committed a
  self-contained tree is exactly as regenerable, from the sentinel's
  perspective, as the one that produced it.
- Any registry failure during materialization fails the whole run. On
  failure, neither `<dir>` nor `<dir>.tar.gz` is left in a partial or
  inconsistent state: the staged replacement is discarded, and whatever
  previously existed (if anything) survives untouched. See Update model for
  the mechanism (staged-then-atomic-swap) that makes this true, rather than
  the delete-first sequencing an earlier draft of this spec described. That
  earlier delete-first framing was self-contradictory in its own failure
  case, so this section states the mechanism plainly rather than only the
  outcome.

## Acceptance

- A test asserting the emitted Containerfile contains no registry reference
  other than the base image. This is the mode's defining invariant.
- A test running `--self-contained` together with `--pin-digests` and
  asserting the invariant above still holds: no fragment `FROM` stage, no
  fragment digest comment, and no COPY/mount `--from=`, while the base
  `FROM` line is pinned to its digest.
- Golden-file test on the emitted Containerfile in self-contained mode.
- Integration test materializing from fixture fragments, asserting the
  archive contents match the tree byte for byte. This composes the real
  materialization path with the real archiving step over one tree, not two
  separate tests that each exercise half the pipeline.
- A test that an existing non-tool-generated `<dir>` is refused, including
  the specific false-positive case a content-only heuristic would have
  missed: a directory containing both `Containerfile` and `fragments/` but
  no sentinel.
- A test that `--self-contained` plus `--ocp` errors.
- A test that `--self-contained` plus `--output` errors.
- A test that regeneration is idempotent: running the command twice against
  the same manifest and registry state produces a byte-identical `<dir>`,
  and the second run's directory check passes against the first run's own
  output.
- A test that a materialization failure leaves a prior tree, if any,
  completely untouched (cleanup-on-failure).
- A test verifying `manifest.yaml` is written with the input manifest's
  content, under that fixed name regardless of the source path.
- A test covering a hooks-only fragment (hooks but no tree content) in
  self-contained mode: materialization produces `fragments/<name>/hooks/`
  and no `fragments/<name>/tree/`, and the emitted Containerfile has the
  context-relative hook mount with no corresponding COPY for that fragment.

## Non-goals

- No OCP interaction.
- No provenance or signature recording. Users verify fragments at generation
  time; afterward, trust belongs to the tree and whatever holds it.
- No lockfile, no partial update, no in-place mutation.
- No archive-suppression flag yet.

## Open for the implementation plan

- Short single-dash flag aliases are tracked separately and out of scope
  here.
