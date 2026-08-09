# Roadmap

osfragment-assemble composes reusable, registry-native build units into a
generated Containerfile. It is a working proof of the format, with example
fragments published and a full composed build verified end to end.

This document is the public record of the project's tracked work. Each entry
summarizes one tracked item: enough to read the intent, not a specification.
Tracks are groupings, not phases, and nothing here is a schedule. Where a
question is open, the document says so rather than guessing. Open work is
listed in the tracks; deliberately parked work is listed at the end with the
condition that revives it.

Last refreshed: 2026-08-09.

## Distribution and packaging

The cheapest available increase in how seriously the project reads. Continuous
integration (formatting, lint, tests on every push and pull request) landed and
is no longer on this list.

- A documented install path. Today the tool requires a Rust toolchain and a
  source checkout. Release binaries are a separate decision.
- Package metadata completeness: the license and repository fields are absent
  from `Cargo.toml`.
- Repository hygiene a first-time reader hits: a `.containerignore`, and
  retiring generated scratch artifacts from the working tree.

## Correctness and safety hardening

A fragment is content authored by someone other than the person building the
image, and these items decide how much trust the tool extends it.

- A byte cap on fragment layer extraction. The largest published example
  fragment is roughly 350 MB, so unbounded extraction is exercised in normal
  use, not only under attack.
- Streaming layer pulls instead of reading every layer into memory at once.
- Whiteout handling in layer aggregation: a file deleted in a later layer
  still counts as present, and `fragment.toml` discovery is first-layer-wins
  while entrypoint discovery is last-wins. One shadowing answer should hold
  everywhere.
- Mirror rewrites are clobbered for fragments carrying both repo files and
  other tree content: the whole-tree copy re-delivers pristine repo files over
  the rewritten ones.
- A mirror declared on a fragment with no repo files is silently ignored,
  against the project's loud-failure stance.
- The repo conflict check hashes a fragment's whole repo-file map, so
  unrelated repo files can false-conflict.
- Schema strictness in `fragment.toml`: the provides and conflicts tables
  silently accept unknown keys, so a typo weakens conflict checking with no
  parse error.
- Run `ldconfig` after fragment assembly, so a shared-library version bump
  cannot leave a stale linker cache on deployed hosts.

## Build mounts and credential material

The build-mounts mechanism shipped. This work hardens the surfaces around it.

- Print derived mount targets at generation time, the same list `inspect`
  shows. Reporting, not validation: a partial file set that silently moves a
  derived mount root becomes visible before the build instead of failing
  minutes into it.
- Update four user-facing strings whose example paths teach a mount target
  that subscribed build hosts silently override. The format documentation's
  worked examples are being corrected in the same direction.
- Local container storage as a fragment source. A locally built, never-pushed
  fragment cannot satisfy the digest-pin rule today; the direction under
  evaluation is computing the pin by performing the copy.
- Evaluate read-only on hook mounts, closing a recorded asymmetry with build
  mounts, which are already read-only.
- Document the transparency-log caveat: signing a credential fragment with a
  default keyless flow publishes its repository name and digest to a public
  log, which cuts against a private-registry custody model.
- State the build-mounts custody boundary in the design explainer now that
  the mechanism has shipped: the tool authenticates package acquisition and
  is not a secrets manager.

## Self-contained mode

The newest output surface, so it carries the most unfinished edges. Behavior
first, then coverage.

- Byte-deterministic archives: re-running produces a byte-identical tree but
  a differing tar.gz, from per-entry timestamps.
- Eliminate repeated layer pulls. Every run re-pulls every fragment, both
  within a run and across runs.
- Cover the symlink case in the early safety gate, before the manifest read
  and fragment pulls rather than after them.
- A rename-aside swap to close the crash window during output replacement.
- Decide the incidental-dotfile refusal policy: a target directory holding a
  `.gitignore` is currently refused as foreign, which lands on the mode's own
  gitops use case.
- A golden Containerfile test with a pinned base digest. Its absence is how
  two header defects landed unnoticed.
- Tightened invariant assertions, a symlink-refusal test asserting the link
  itself survives, and an archive-failure test asserting no temp-file debris.
- Name the output archive path in the archive tempfile-creation error.

## Generation, schema, and CLI

Individually minor, collectively the surface someone else has to learn.

- Validate or drop the manifest `apiVersion` field. Any string currently
  parses silently, including retired ones.
- A generation mode enum replacing the boolean pair that distinguishes
  standard, on-cluster, and self-contained output.
- Separate the planner from the emitters. The one architectural item here:
  it is what lets a second emission target exist without a redesign.
- Header consistency: remaining refinements so version and manifest reporting
  cannot diverge between output paths.
- Short single-dash aliases for the common flags.
- Scrub the remaining em dashes from user-facing strings; one ships inside
  every generated Containerfile that reports a path collision.

## The OpenShift on-cluster path

Emitting a MachineOSConfig for on-cluster builds is a requirement the rest of
the ecosystem does not carry. The v1 API migration and the architecture
casing fix shipped; what remains needs a design decision or a live cluster.

- Design for the 4096-character ceiling on the MachineOSConfig Containerfile
  field. An eight-fragment composition already sits near the wall.
- Confirm a live on-cluster build accepts the current hook mount form.

## Examples and documentation

The examples are the argument: every claim the format makes is either
demonstrated by a fragment someone can pull, or it is a claim.

- Re-lead the worked example and the minimal manifest with fragments that
  demonstrate more than repository configuration.
- Complete the driver fragment pair. The vendor-installer half works; the
  conventional repository-based counterpart does not exist, so a declared
  conflict currently points at a fragment that is not there.
- Decide multi-architecture publishing. Every published example fragment is
  arm64, so the position that the tool inherits multi-architecture support
  from OCI is untested.
- A rendered self-contained Containerfile in the docs. Every hook line the
  docs currently show carries a registry reference, because only registry
  mode is shown.
- Document the `provides.repos` field in the format specification; today it
  appears only as an annotation key.
- A rationale entry on why build-only toolchain packages belong in a hook's
  entrypoint rather than the declared package list.
- Process-docs hygiene: correct several recorded claims that drifted from the
  code.

## Test suite

- Split layout reading from fetching in the layer-pull path, so OCI-layout
  parsing is fixture-testable with no network.
- Make the list command testable, so its manifest-reporting line can be
  pinned by a test.
- Extract the repeated mixed-fragment test fixture into a shared helper.
- Replace an exact permission-mode assertion that in practice asserts the
  developer's umask.
- Container-based hook execution tests, once continuous integration runs real
  container builds.

## Exploration and upstream

- Whether the tool should also run inside the container build it contributes
  to is open and undesigned. Emitting a Containerfile from inside the build
  executing it is not obviously coherent, so the answer likely involves doing
  something other than emission in that mode. Self-contained mode is
  adjacent, producing a portable context, and adjacent is not sufficient.
  The question is listed so this document does not imply it was dismissed.
- Explore a fragment-to-sysext emitter: the same captured content targeting a
  system extension image as a second output.
- Prototype the reverse direction: flatten a published system extension into
  a bootc image build and measure what survives.
- An upstream crash report: subscription-manager's certificate parser
  segfaults on a colon between PEM markers, with a small recorded reproducer.
  Found while constructing placeholder credential material.

## Parked, with revive conditions

- Rechunking support. Designed, specified, and reviewed, then parked: the
  emitted pattern needs a builder version that is not yet the normal
  installed base. Revive when it is, and reevaluate the specification fresh
  rather than resuming it.
- A Debian-family backend. The format is already family-agnostic; the
  coupling is the emitted package transaction and the repository payload
  convention, so a second family is a second emission target rather than a
  format redesign. Revive when a Debian-family bootable-container base image
  exists that people actually use.
- A kickstart-carrying example fragment, demonstrating that an existing
  declarative configuration format can be carried and applied without the
  tool learning that format. Its earlier technical blocker is resolved; kept
  for research value, deliberately not sequenced.
