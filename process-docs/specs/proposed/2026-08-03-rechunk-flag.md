# The `--rechunk` Flag

**Status:** Proposed
**Date:** 2026-08-03

An opt-in `--rechunk` flag on the default generate command. When set, the
emitted Containerfile gains a rechunk phase: the assembled image is handed to
[chunkah](https://github.com/coreos/chunkah) inside the same build, and the
chunked result becomes the final image. The tool emits stages and nothing else
— it does not run chunkah, does not inspect an image, and does not wrap the
builder.

**Why.** A bootc image assembled from a base plus N fragments inherits the
base's layer structure and adds a small number of large layers on top. Content-
based layer splitting produces layers that track how content actually changes,
so an upgrade transfers less and shared content deduplicates across images.
chunkah is the general successor to `rpm-ostree compose build-chunked-oci` and
is where that capability lives now.

**Why in the Containerfile.** chunkah's README documents two integration
shapes: split an existing image after the fact, or split at build time from
inside the Containerfile — it calls the second the "`FROM oci:` trick" and
ships a bootc-specific worked example of it that already contains
`RUN bootc container lint`. Projects that own a build pipeline invoke chunkah
from that pipeline; projects that ship a Containerfile put chunkah in the
Containerfile. This tool produces a Containerfile and nothing else, so the
second shape is not merely viable for it, it is the only one consistent with
what the tool is.

## The flag

`--rechunk`, boolean, default off, on the default (no subcommand) generate
path. Not a manifest field: like `--pin-digests`, it describes how this
invocation emits, not what the composition is.

The name is the ecosystem's verb for the operation, deliberately not the name
of the tool that performs it. The emitted stage is free to change tools; the
flag is not.

## Two refusals

Both are hard errors at generation time, before anything is written.

### `--rechunk` with `--ocp`

```
--rechunk cannot be combined with --ocp: the on-cluster builder cannot run
this pattern. FROM --after= requires buildah >= 1.44.0 (2026-05-27), and no
current OCP release ships it. Generate without --rechunk for the OCL path.
```

Refusing rather than warning, and refusing rather than emitting, is the whole
point. MCO validates a `MachineOSConfig` with `openshift/imagebuilder`, whose
pinned version in MCO's `go.mod` (v1.2.21, read 2026-08-03) is past v1.2.20
where `FROM --after=` parsing landed. So a `MachineOSConfig` carrying this
pattern is **accepted by the API server and then fails inside the build pod**.
Accept-then-fail is the failure shape this project refuses to ship. A warning
would produce exactly that object.

Two further blockers, independent of the parser asymmetry: MCO's
`buildah-build.sh` builds its argument list without `--skip-unused-stages=false`
and the user cannot add one, and the OCP emitter's `FROM configs AS final`
convention assumes `final` is the last stage, which a rechunk tail would break.
Neither is load-bearing for the refusal; both are reasons the refusal will not
become unnecessary soon.

**Where the check lives: `main.rs`, at the top of the assembly arm, before the
manifest is read and before `check_target_dir_safe` runs.** Not in the
generator: `generate_containerfile(ocp: true)` is only reached *after* the
standalone Containerfile has already been written to `--output`, so a
generator-level refusal would fire after a side effect. Not clap's
`conflicts_with` either, despite the `--self-contained` precedent: it fires at
the right time but its message is a fixed usage line and cannot explain why.
An `anyhow::bail!` matches how `check_target_dir_safe` refuses.

**Revisit when OCP ships buildah >= 1.44.0.** This is version lag with a known
finish line, not a design dead end.

### `--rechunk` on a base that does not classify as bootc

```
--rechunk requires a bootc base image, but <base> is classified as a plain
container image. The rechunk phase prunes /sysroot/ and re-applies
containers.bootc=1, neither of which is meaningful here. Remove --rechunk, or
correct baseType in the manifest.
```

An explicit opt-in must not silently no-op. Dropping the bootc-specific
arguments and chunking anyway would produce something the user did not ask for
under a flag that says nothing about it.

**Precision on when this actually fires.** `classify_base` defaults to `Bootc`
in every ambiguous case: manifest override absent *and* the `containers.bootc`
label absent still yields bootc capabilities, as does a failed `skopeo`
lookup. `capabilities_for_base_type(BaseType::Container)` is the only source of
an empty capability set, and it is reached only through an explicit
`baseType: container` in the manifest. So this refusal fires exactly on the
declared-plain case.

The undeclared-plain case — a genuinely non-bootc base with no label and no
manifest override — passes this check and is caught one layer down: the
`assembled` stage still ends in `RUN bootc container lint`, which fails the
build before chunkah ever runs. The result is a loud build failure, not a
mislabeled image. That is the existing classifier's deliberate posture and
`--rechunk` inherits it rather than adding a second probe.

**Where the check lives: the generator**, which already receives
`capabilities`. It has no destructive side effect to precede — in
`--self-contained` mode the generator runs before `write_output`, so bailing
here leaves the target directory untouched.

## Emitted structure

Two changes to the existing emission, plus a new phase at the tail.

**1. A global `ARG`, before the first `FROM`.** Build args must be declared
before any stage to be visible for re-declaration inside one. This lands
immediately after the header comment block and before the fragment stages:

```dockerfile
# Optional: --build-arg CHUNKAH_CONFIG_STR="$(podman inspect <base>)" carries
# every base image label through the rechunk. Without it, only
# containers.bootc is re-added.
ARG CHUNKAH_CONFIG_STR
```

The `ARG` is inert when unset. It exists so a user who wants full label
preservation can have it without editing the generated file; see Trade-offs.
The comment is not optional — a bare `ARG` at the top of the file with no
explanation is worse noise than the two lines that explain it.

**2. The main stage is named.** `FROM <base_ref>` becomes
`FROM <base_ref> AS assembled`, only when `--rechunk` is set. `<base_ref>` is
computed exactly as today, digest-substituted under `--pin-digests`.

No stage-name collision is possible: fragment stages are emitted as
`frag-<name>`, so no fragment can claim `assembled` or `chunkah`.

**3. `RUN bootc container lint` does not move.** It is already the last
instruction of the main stage, which is already where upstream's bootc example
puts it — before the chunker consumes the rootfs. With `--rechunk` that stage
becomes `assembled` and the lint line stays exactly where it is.

No second lint is added against the chunked output. Upstream does not, the
rootfs content is byte-identical (chunkah's stated property is that content is
never modified apart from mtime), and an instruction after the final `FROM`
deepens the annotation loss described below.

**4. The rechunk phase**, appended after the validation phase, separated by a
blank line (the validation phase currently emits no trailing blank):

```dockerfile
# --- Rechunk ---
# Requires podman >= 6.0.0 / buildah >= 1.44.0. Earlier builders do not
# recognize FROM --after= and fail with:
#   FROM only supports the --platform flag
FROM quay.io/coreos/chunkah:v0.6.0 AS chunkah
ARG CHUNKAH_CONFIG_STR
RUN --mount=from=assembled,src=/,target=/chunkah,ro \
    --mount=type=bind,target=/run/src,rw \
      chunkah build \
        --prune /sysroot/ \
        --max-layers 128 \
        --output oci:/run/src/rechunk.oci

FROM --after=chunkah oci:rechunk.oci
LABEL containers.bootc=1
```

Section-header style follows the file's existing `# --- Name ---` convention.
(The validation phase's `# --- Phase: validation (90) ---` is a survivor of the
dropped `phase` field and the sole outlier; this spec does not touch it.)

**The floor comment is emitted; the consumer floor is not.** The generated
Containerfile is a handoff artifact, and the person who builds it is the person
the builder floor bites. The person the *consumer* floor bites is deploying the
resulting image and may never see this file, so `bootc >= 1.1.3` belongs in
`--help` and the README instead. Two floors in two places, each where its
audience is. This is a decision, not an omission — do not add the consumer
floor to the generated output later on the theory that it was overlooked.

**On `src=` rather than `source=`.** The chunker's rootfs mount uses `src=`,
which is what chunkah's README and its live consumers write, and which buildah
accepts as an alias of `source=`. The hooks emission in the same file uses
`source=`. The inconsistency is deliberate: the chunker line is copied verbatim
from the upstream-verified pattern and should stay diffable against it.

### The byproduct directory is named `rechunk.oci`

Decided 2026-08-03. Upstream's examples write `out`, and this spec deliberately
does not.

**`out` is an example argument, not a protocol name.** An OCI layout is
identified by its contents — `oci-layout` and `index.json` — never by its
basename, and the verification that this pattern works covered the *mechanism*
(a relative `oci:` path resolved against the build context), which applies to
any relative path. There is nothing to be compatible with here.

**Two reasons the distinctive name is better**, and the first is the one that
matters:

- **Collision.** The byproduct lands in whatever directory the user builds
  from. In `--self-contained` mode that is the tool's own output directory; in
  registry mode it is an arbitrary user directory, where a generic `out/` is at
  its worst — that is exactly the name a user's own build output already has.
  `rechunk.oci` collides with essentially nothing in either mode.
- **Self-describing on disk.** Someone who finds it a week later can tell what
  wrote it and why without reading the Containerfile.

**Deliberately visible, not dot-hidden.** The directory is an image-sized copy
of the rootfs. Something that large should not accumulate invisibly.

Because it is visible and large, `--rechunk`'s `--help` and the README flag
list say it exists: a build leaves a `rechunk.oci/` OCI layout in the build
context, it is safe to delete, and it is not cleaned up automatically. The tool
cannot remove it — the tool has exited long before the build runs.

**Both emission sites change together** — the chunker's `--output` argument and
the final stage's `FROM` — and both take the name from the same constant. They
are the same path expressed from two sides of a bind mount, and a plan that
changes one without the other produces a Containerfile that fails at the final
`FROM` with a missing layout.

One consequence to know rather than act on: `oci:` names a *directory* while
the `.oci`/`.ociarchive` suffixes elsewhere in this ecosystem usually mark
archive *files*. The name is still the right call; it just is not evidence
about the transport.

### Mechanism, stated once

`FROM --after=chunkah oci:rechunk.oci` is not a stylistic choice.
`oci:rechunk.oci` is a local transport resolved relative to the build context,
and buildah cannot infer that an earlier stage produces it. Without `--after`, the pattern requires the
*builder* to be invoked with `--skip-unused-stages=false` — a CLI flag a
Containerfile cannot carry, which would make this feature depend on
out-of-band instructions to the user. `--after` declares the dependency inside
the file, which is what makes the feature emittable at all.

Two properties were verified by local build on buildah 1.44.0 (aarch64, Fedora
rawhide, 2026-08-03) rather than inferred: an `--after`-referenced stage
survives unused-stage pruning under a plain `buildah bud` with no extra flags
(a genuinely unreferenced stage in the same file was pruned in the same run,
as a control), and the same holds when the final `FROM` is the local-transport
`oci:<dir>` form, with the producer stage completing before the final `FROM` is
evaluated. The verification used `oci:out`; what it established is that a
relative `oci:` path is resolved against the build context, which is a property
of the transport and not of the basename.

The final stage is not derived from the assembled image — it *is* the
chunkah-produced image. That is the mechanism by which the chunked output
becomes the build's result.

`chunkah build` writes an OCI directory layout to `/run/src/rechunk.oci`, i.e.
`rechunk.oci/` inside the build context. This leaves that directory behind
after a build, in every mode; see the `--self-contained` interaction below,
where it has a consequence.

### Named constants

The chunker image reference, the tag, the layer cap, and the prune path are
each named constants in the generator, not inline literals. The tag in
particular is a maintenance surface: chunkah is 0.x, shipped nine releases in
four months, and changed its own recommended Containerfile form one release
ago (`--output oci:` arrived in v0.6.0, 2026-06-08, and the release notes
encourage switching to it). Bumping the pin is a deliberate, tested change.
Emitting `:latest` would silently change behavior on the next release and is
never correct here.

## Recorded trade-offs

**Layer annotations are dropped. Accepted.** Any instruction after the final
`FROM` — including the `LABEL` — loses layer annotations inherited from the
parent image (containers/buildah#6652, open, last updated 2026-07-03). chunkah
describes those annotations as informational. `containers.bootc=1` is required
by bootc and is lost by the build-time flow, so re-applying it is not optional:
without the label the image is not a bootable container. Trading cosmetic
annotations for a required label is the correct trade, and it is recorded here
so it reads as a decision rather than an accident.

**Full label preservation via `CHUNKAH_CONFIG_STR` is declined.**
`--build-arg CHUNKAH_CONFIG_STR="$(podman inspect $BASE)"` preserves every base
label including versioning metadata. Emitting a Containerfile that requires it
would mean the tool either runs `podman inspect` itself — crossing the line the
project exists to refuse — or ships a file that only builds when the caller
remembers a precomputed argument. Both are worse than losing labels. The
declared `ARG` is the escape hatch: a user who wants full metadata supplies it
and nothing in the file changes.

Two consequences worth knowing if that hatch is used: `chunkah build` then
wants `--label ostree.commit- --label ostree.final-diffid-` to strip labels
carried in by the config, and a large config string can exceed the environment
size limit (chunkah#136, closed 2026-06-01, with `--config` reading from a file
as the escape). Neither is emitted by default, because with no config supplied
all labels are already gone and those arguments are no-ops. Both belong in
`--help` rather than in unconditional emission.

**SELinux xattrs are not persisted by the chunk flow.** chunkah does not
currently write `security.selinux` xattrs at all (chunkah#159, opened
2026-07-27), relying on bootc's client-side relabeling. This is the status quo
for chunked images generally and not something this tool introduces, but it is
a real property of turning the flag on and belongs in the flag's documentation
rather than being discovered later.

## Flag interactions

### `--pin-digests`

Pins the chunker image too. The chunkah image is a build-time input pulled from
a registry — the same category as the base image, and arguably more security-
relevant, since it is a binary that rewrites the entire image rather than a
payload of files. Resolve `quay.io/coreos/chunkah:v0.6.0` to a digest at
generation time with the same `resolve_digest` call the base uses, and emit
`quay.io/coreos/chunkah@sha256:…` with the tag stripped, exactly as
`split_image_ref` already handles the base.

This adds a second registry round-trip to `--pin-digests --rechunk`. Expected
and worth stating: the flag combination costs one more network call at
generation time.

Unpinned, the emitted ref stays `quay.io/coreos/chunkah:v0.6.0`.

### `--self-contained`

**Permitted.** A chunker image pull is one more build-time registry pull, in
the same category as the base image pull that already exists in this mode.
Refusing the combination would be over-restrictive and inconsistent with the
base-image precedent.

Two wrinkles, both real, both in `self_contained.rs`.

**Wrinkle 1 — `oci:rechunk.oci` is excluded from context-path validation by
accident, not by rule.** `context_paths_referenced` (`self_contained.rs:592-601`)
collects build-context paths by taking every whitespace/comma-delimited token,
stripping an optional `source=` prefix, and keeping those that begin
`fragments/`. `oci:rechunk.oci` does not match that prefix and is silently
ignored. That outcome is correct — the layout is produced by the build, not by
materialization, so demanding it on disk would be wrong — but it holds only
because of the prefix filter. The hook-command scan in the same test
(`self_contained.rs:742-768`) keys on `source=fragments/` and skips the
chunker's mounts for the same accidental reason: neither chunker mount carries
a `source=` at all.

**Requirement:** when a `--rechunk` case is added to
`emitted_containerfile_paths_resolve_in_the_materialized_tree`, the exclusion
must be made explicit — at minimum a comment at the filter stating that
build-produced paths are deliberately out of scope. Otherwise a later
generalization of the helper (say, "collect every non-absolute path token")
starts demanding `rechunk.oci/` on disk and fails for a reason nobody will
recognize.

**Wrinkle 2 — a rechunk build leaves `rechunk.oci/` in the output directory,
and the sentinel guard then refuses to regenerate into it.** In this mode the
build context *is* `<dir>`, so `chunkah build --output oci:/run/src/rechunk.oci`
writes `<dir>/rechunk.oci`. `TOOL_GENERATED_ENTRIES` (`self_contained.rs:33-38`)
is exactly `Containerfile`, `manifest.yaml`, `fragments`,
`.osfragment-assemble`, and `check_target_dir_safe` requires *every* entry in
the directory to be recognized. So the next
`osfragment-assemble --self-contained <dir> --rechunk` refuses, and today it
refuses with a message that names a cause that is not the cause:

```
--self-contained target <dir> already exists and was not generated by this
tool …
```

The guard is behaving correctly — `rechunk.oci/` was not written by this tool —
but the obvious build-then-regenerate loop hits this every time, and the
message sends the user looking for a foreign directory rather than at their own
last build.

**Resolution, decided 2026-08-03: extend the error message, change no
behavior.** `check_target_dir_safe` still refuses in exactly the cases it
refuses in today. When `rechunk.oci` is among the unrecognized entries, the
message names it as a `--rechunk` build byproduct and tells the user to remove
it and rerun:

```
--self-contained target <dir> already exists and was not generated by this
tool (expected the .osfragment-assemble sentinel plus Containerfile,
manifest.yaml, fragments/, and nothing else). It contains rechunk.oci/, the
OCI layout a --rechunk build writes into its build context. Remove
<dir>/rechunk.oci and re-run, or point --self-contained at a new directory.
```

The distinctive byproduct name is what makes this message possible: the guard
can say what the entry is because only a rechunk build produces something by
that name. With a generic `out/` the tool could only guess.

This keeps the guard content-blind, which is the property that makes it
trustworthy. It reads directory entries by name and authorizes nothing on the
basis of what any file contains.

**Rejected: adding `rechunk.oci` to `TOOL_GENERATED_ENTRIES`.** That constant
means "entries the tool itself may have written," and this is not one — the
builder wrote it, on a later and separate run. Adding it would make the tool
silently delete an image-sized build artifact on *every* regeneration, not just
rechunk ones, widening a deliberately narrow guard to cover something outside
it. A loud refusal the user resolves with one `rm` is the better failure.

### Documentation wording

The claim that a self-contained build needs no registry except the base image
appears in four places and becomes conditionally false under `--rechunk`. It
is corrected in three of them:

| Location | Current text |
|---|---|
| `src/main.rs:49` (`--self-contained` help) | "needs no registry access at build time except for the base image" |
| `README.md:169` | "references no registry image except the base" |
| `src/self_contained.rs:4` (module doc) | "references no registry image except the base" |

Each gains the chunkah image **as a condition, not a blanket addition**: without
`--rechunk` there is no chunker pull and the original claim is exactly true. An
unconditional "except the base image and the chunkah image" would be wrong in
the common case. The `main.rs` help text becomes:

> needs no registry access at build time except for the base image, and the
> chunkah image when `--rechunk` is used

and the other two sites take the same conditional clause in their own phrasing.

The fourth site, the `--self-contained` entry in `CHANGELOG.md`, is a record of
what that flag did when it was added and is not rewritten; the new `--rechunk`
entry carries the correction.

This is a deliberate wording change shipped with the flag, not after it. That
sentence has been over-claimed once already — it was stated as "no network,"
which is puncturable from the tool's own `--help` by exactly the audience the
line is aimed at. Precision here is cheap and the failure mode is not.

## Documented floors

These feed `--help` for `--rechunk` and the README's flag list. **The tool
never probes the user's builder version.** It generates; it does not wrap, and
it does not interrogate the environment it generates for. These are documented
requirements, not runtime checks.

**Builder: podman >= 6.0.0 / buildah >= 1.44.0.** `FROM --after=` shipped in
buildah v1.44.0 (2026-05-27). Verified from `go.mod` at each podman release tag
(2026-08-03): podman v6.0.0 (2026-06-24) is the first release to vendor it.
Every podman 5.8.x — including v5.8.5 (2026-07-08), which is *newer* than
v6.0.0 — still vendors buildah 1.43.2 and cannot build this pattern. "Update
podman" is therefore not sufficient advice; the requirement is the 6.x line.

Failure on an older builder is loud and unmistakable. Verified against podman
5.8.4: buildah parses the file, builds the first stage, and errors on the final
`FROM` with `FROM only supports the --platform flag`, exit 125. Wasted work,
clear message, **no silent fallback to an unchunked image**. That is what makes
the floor a documentation problem rather than a correctness trap, and it is the
opposite of the OCL asymmetry that drives the `--ocp` refusal.

Also worth documenting for anyone reproducing this: no official container image
could build the pattern as of 2026-08-03 — `quay.io/buildah/stable` was at
v1.43.1 and `quay.io/podman/stable` at v5.8.2, both below the floor.

**Not verified, stated as such:** buildah 1.44.1 (what podman 6.0.2 actually
vendors), x86_64, and `podman build` as the front end. All three verification
runs used `buildah bud` directly on aarch64 with buildah 1.44.0. Podman's
`build` is a thin wrapper over the same vendored executor, so no difference is
expected, but none was observed.

**Consumer: bootc >= 1.1.3 on every system that pulls the resulting image.**
This is the rationale for `--max-layers 128` and not a larger value: chunkah
documents a minimum bootc of v1.1.3 for <= 128 layers and v1.9.0 for more, so
128 keeps the fleet floor four minor versions lower. Upstream also recommends
raising the default from 64, so 128 is both the recommended value and the one
with the cheaper deployment requirement. This is a fleet-wide constraint —
users need to know it before flipping the flag, not after.

**SELinux:** see chunkah#159 under Trade-offs.

## Tests

Generator unit tests, mirroring the shape of the existing lint suite
(`container_base_omits_bootc_steps`, `bootc_base_preserves_both_steps`): build
a manifest and fragments, call `generate_containerfile`, assert on the emitted
string.

- **Flag on, bootc base.** Output contains `FROM quay.io/coreos/chunkah:v0.6.0 AS chunkah`,
  `FROM --after=chunkah oci:rechunk.oci`, `LABEL containers.bootc=1`,
  `--prune /sysroot/`, `--max-layers 128`, and the base line carries
  `AS assembled`.
- **The byproduct path agrees on both sides.** The chunker's `--output`
  argument and the final stage's `FROM` name the same layout — assert
  `--output oci:/run/src/rechunk.oci` and `FROM --after=chunkah oci:rechunk.oci`
  together, not each alone. They are one path expressed from two sides of a
  bind mount, and a change to one without the other yields a Containerfile that
  builds the chunker and then fails at the final `FROM`. Deriving both from the
  same constant is what the test pins.
- **Lint stays in the assembled stage.** `RUN bootc container lint` appears
  *after* the `AS assembled` line and *before* the chunker `FROM`. A
  `contains` assertion alone is not sufficient — it passes just as happily if
  the lint migrates to the final stage, which is the regression worth pinning.
- **`ARG` placement.** `ARG CHUNKAH_CONFIG_STR` appears before the first `FROM`
  in the output. Assert on index, not presence: a correctly-spelled `ARG` in
  the wrong place is invisible to the chunker stage and produces no error.
- **Flag off.** None of `chunkah`, `--after`, `rechunk.oci`, `AS assembled`
  appear, and every other assertion the existing suite makes still holds. This
  is what keeps the flag genuinely opt-in.
- **Non-bootc base plus flag → error.** Empty capability set plus `--rechunk`
  returns `Err`, and the message names the flag and the base image.
- **`--ocp` plus flag → error.** The refusal is a `main.rs` check, so extract it
  as a testable function in the style of `should_keep_fragment_digests` and
  assert both directions. The message must name the OCL builder, not just the
  conflict.
- **`--pin-digests` plus flag.** The chunker ref is emitted as
  `quay.io/coreos/chunkah@sha256:…` and the tag form
  `quay.io/coreos/chunkah:v0.6.0` is **absent**. Asserting only that the digest
  form is present would pass on output containing both.
- **`--self-contained` plus flag.** The chunk phase is emitted, and
  `oci:rechunk.oci` is not collected by `context_paths_referenced`. This test
  is what makes the exclusion deliberate rather than incidental.
- **The guard names the byproduct.** A directory holding the sentinel, the
  usual tool-generated entries, and a `rechunk.oci/` is refused by
  `check_target_dir_safe`, and the error names `rechunk.oci` and the remedy.
  Pair it with an unchanged-behavior control: a directory holding the sentinel
  plus some *other* foreign entry is refused with the original message, which
  is what proves the message branched and the guard did not.
- **The floor comment is emitted.** The generated phase contains the
  podman/buildah floor line. Cheap, and it is the only thing standing between
  a future cleanup pass and a generated file that no longer documents its own
  requirements.

**Live end-to-end build is manual-only for now.** Verifying that the emitted
file actually produces a chunked bootc image requires podman 6.x, which the
project's own development machine does not have — every podman 5.8.x hard-
errors on `FROM --after=`. The unit tests pin the emitted *text*; the
`--after` mechanism underneath it was verified separately by local build
(see Mechanism). Closing the remaining gap — chunkah's own behavior on a real
bootc base with `--prune /sysroot/` and the label re-application — is a manual
step to run once a 6.x builder is available, and it is the one thing no test
here covers.

## CHANGELOG

Under `### Added`, non-breaking: `--rechunk` emits a chunkah rechunk phase in
the generated Containerfile; the assembled stage is named `assembled` and the
chunked output becomes the final image. Note the builder floor
(podman >= 6.0.0 / buildah >= 1.44.0), the consumer floor (bootc >= 1.1.3),
the refusal with `--ocp`, that under `--self-contained` a build now also pulls
the chunkah image, and that a build leaves a `rechunk.oci/` OCI layout in the
build context which is safe to delete and is not cleaned up automatically.

Under `### Changed`, if the guard message lands as its own change:
`check_target_dir_safe` now names `rechunk.oci/` as a `--rechunk` build
byproduct when it finds one, instead of reporting only that the directory was
not tool-generated. No change to which directories it accepts.

Nothing is breaking: with the flag off, emission is byte-identical to today.
Shipped examples and generated documentation samples need no regeneration.

## Out of scope, recorded

- **`--ocp` support.** Refused above. Revisit when OCP ships buildah >= 1.44.0.
- **Rechunking only layers added since `FROM`** (chunkah#127, opened
  2026-05-07). Almost certainly the *better* mode for this tool — it would chunk
  only fragment-added content and leave base layers intact, preserving
  cross-image base reuse. It would change the emitted arguments. **This is the
  watch item.** Adopting it later is cheap under the project's
  no-backwards-compatibility policy: change the constant and the argument list,
  rebuild the examples. Do not design around a feature that does not exist.
- **Full label preservation.** Declined above; the `ARG` is the escape hatch.
- **Probing the user's builder version.** Never. The tool generates; it does not
  interrogate the environment its output will run in. Documented floors are the
  whole mechanism.
- **A `--rechunk-max-layers` knob.** 128 is upstream's recommendation and carries
  the lower consumer floor. Wait for someone to need otherwise.
- **cosign verification of the chunker image.** chunkah images are keyless-
  signed and a real supply-chain story exists, but it is a separate feature
  with its own surface.
- **Fragment-declared layer grouping.** chunkah supports `user.component` and
  `user.update-interval` xattrs for grouping, and a fragment declaring its own
  update cadence so chunkah packs it sensibly is a genuinely interesting future
  fit between the two formats. Not v1.

## Open for the implementation plan

- **How the new emission inputs reach the generator.** `generate_containerfile`
  already takes six positional parameters, two of them bare `bool`s (`ocp`,
  `self_contained`), and `--rechunk` adds at least a flag plus an optional
  resolved chunker digest. Options: two more positional parameters (consistent,
  and pushes the signature to eight); a small options struct for the rechunk
  inputs only; or an options struct for the whole call. Not decided here — it
  is a code-shape question, and the behavior above is the same under all three.
- **Whether the `--ocp` refusal is a plain `bail!` in the assembly arm or an
  extracted predicate.** The tests above want it extracted; the plan confirms
  the shape.

Settled, recorded here so the plan does not reopen them: the chunker image is
pinned to `v0.6.0` and never `:latest`; `--max-layers` is 128 and the reason is
the consumer bootc floor, not packing quality; `bootc container lint` does not
move and is not duplicated; the builder floor is emitted as a comment and the
consumer floor is not; both refusals are errors rather than warnings; the
byproduct directory is `rechunk.oci`, named from one constant used by both
emission sites, visible rather than dot-hidden; and `check_target_dir_safe`
keeps its current behavior exactly, gaining only a branch in its message text.
