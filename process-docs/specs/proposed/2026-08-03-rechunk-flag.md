# The `--rechunk` Flag

**Status:** Parked at spec (2026-08-03). Revised after review round 1; round 2
deferred to revival. No implementation plan.
**Date:** 2026-08-03

**Why parked.** The builder floor below — buildah >= 1.44.0 server-side, i.e.
podman 6.x — is in almost nobody's installed base yet, including the official
`quay.io/podman/stable` and `quay.io/buildah/stable` images and the normal macOS
podman install path. The feature is a frill until that changes. This is a
timing call, not a dispute with the design: the floor is correct, the emission
is correct, and the spec is complete as written.

**Revive when** buildah >= 1.44 reaches the official podman/buildah images *and*
the mainstream macOS podman install path. At that point: resume review round 2,
and run the parked podman-6 sufficiency build, which closes five **Inferred**
rows in the table below plus the zero-instruction-final-stage question.

The body below is unchanged from the post-review revision and needs no rework to
be picked up.

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

Both are hard errors raised in `main.rs` immediately after `parse_manifest`,
before the base digest is resolved, before any fragment is pulled, and before
anything is written. Both are `anyhow::bail!`, matching how
`check_target_dir_safe` refuses.

**Precedence, when both apply** (`--rechunk --ocp` on a manifest declaring
`baseType: container`): the `--ocp` refusal wins, because it is checked first.
Deterministic, and stated so it is a decision rather than an artifact of line
order. The `--ocp` message is the more useful of the two here — it names a
combination that cannot work at all, where the base-type message names one the
user can fix.

### `--rechunk` with `--ocp`

```
--rechunk cannot be combined with --ocp: the on-cluster builder cannot run
this pattern. FROM --after= requires buildah >= 1.44.0 (2026-05-27), which
OCP's build pods are not expected to ship for some time. Generate without
--rechunk for the OCL path.
```

Note the wording on OCP's builder version. That OCP does not ship
buildah >= 1.44.0 is an inference — from OCL running the RHEL 9 buildah RPM plus
the general upstream-to-cluster lag — not a measurement; nobody has pinned a
buildah version inside an OCL build pod. The message says "not expected to ship
for some time" rather than asserting a fact nobody checked. The rest of the
refusal's reasoning is measured and current.

Refusing rather than warning, and refusing rather than emitting, is the whole
point. MCO validates a `MachineOSConfig` with `openshift/imagebuilder`, whose
pinned version in MCO's `go.mod` (v1.2.21, re-verified 2026-08-03) is past
v1.2.20 where `FROM --after=` parsing landed. So a `MachineOSConfig` carrying
this pattern is **accepted by the API server and then fails inside the build
pod**. Accept-then-fail is the failure shape this project refuses to ship. A
warning would produce exactly that object.

Two further blockers, independent of the parser asymmetry: MCO's
`buildah-build.sh` builds its argument list without `--skip-unused-stages=false`
(re-verified 2026-08-03) and the user cannot add one, and the OCP emitter's
`FROM configs AS final` convention assumes `final` is the last stage, which a
rechunk tail would break.

**Why `main.rs` and not the generator.** `generate_containerfile(ocp: true)` is
only reached *after* the standalone Containerfile has already been written to
`--output`, so a generator-level refusal would fire after a side effect. Not
clap's `conflicts_with` either, despite the `--self-contained` precedent: it
fires at the right time but its message is a fixed usage line and cannot
explain why.

**Revisit when OCP ships buildah >= 1.44.0.** This is version lag with a known
finish line, not a design dead end.

### `--rechunk` on a base declared non-bootc

```
--rechunk requires a bootc base image, but the manifest declares
baseType: container. The rechunk phase prunes /sysroot/ and labels the result
containers.bootc=1, neither of which is meaningful here. Remove --rechunk, or
correct baseType in the manifest.
```

An explicit opt-in must not silently no-op. Dropping the bootc-specific
arguments and chunking anyway would produce something the user did not ask for
under a flag that says nothing about it.

**The trigger is `manifest.base_type == Some(BaseType::Container)`,** which is
known at parse time. That is why this check sits beside the `--ocp` one instead
of after classification: placing it after `classify_base` would mean
`--rechunk --pin-digests` on a declared-container manifest performs a base
digest resolution plus one registry pull per fragment and *then* refuses on a
condition that was decidable before the first round-trip. Both refusals now
share one location, one style, and one place for the precedence rule above.

**Precision on when this actually fires.** `classify_base` defaults to `Bootc`
in every ambiguous case: manifest override absent *and* the `containers.bootc`
label absent still yields bootc capabilities, as does a failed `skopeo` lookup.
`capabilities_for_base_type(BaseType::Container)` is the only source of a
capability set without `Bootc`, and it is reached only through an explicit
`baseType: container`. So this refusal fires exactly on the declared-plain case.

The undeclared-plain case — a genuinely non-bootc base with no label and no
manifest override — passes this check and is caught one layer down: the
`assembled` stage still ends in `RUN bootc container lint`, which fails the
build before chunkah ever runs. The result is a loud build failure, not a
mislabeled image. That is the existing classifier's deliberate posture and
`--rechunk` inherits it rather than adding a second probe.

### The generator's contract check

Separately from the CLI refusal, `generate_containerfile` returns `Err` when
asked to emit the rechunk phase for a capability set that does not contain
`Capability::Bootc`.

This is not a duplicate of the CLI check and is not keyed the same way. The CLI
check is a user-facing refusal on a manifest field, with an actionable message,
placed early to avoid paying for registry round-trips first. This is a contract
check on a `pub fn` in a lib crate, and it is keyed on
**`!capabilities.contains(&Capability::Bootc)`** — the same predicate that gates
`RUN bootc container lint`, which is the safety net the refusal's reasoning
depends on.

The two predicates are equivalent for every CLI-reachable input today, because
`capabilities_for_base_type` has exactly two arms. They are not equivalent in
general: the repo already constructs a Systemd-without-Bootc set in
`systemd_only_emits_preset_but_not_lint`, and that set is a reachable public-API
input. Under it, a check keyed on "capability set is empty" would not fire, no
lint would be emitted, and the rechunk phase would prune `/sysroot/` and label a
non-bootc image — the exact outcome the refusal exists to prevent, with its
stated safety net absent. Keying on the lint gate itself is correct by
construction under any future base type.

## Emitted structure

Three changes to the existing emission, plus a new phase at the tail.

**1. A global `ARG`, before the first `FROM`.** Emitted between the header
comment block's own trailing blank line and the `# --- Fragment stages ---`
block, with its own trailing blank:

```dockerfile
# Override summary: no file path collisions detected
                                    <- the header block's existing trailing blank
# Optional: --build-arg CHUNKAH_CONFIG_STR="$(podman inspect <base>)" carries
# every base image label through the rechunk. Without it, chunkah applies
# containers.bootc=1 and no other label survives.
ARG CHUNKAH_CONFIG_STR
                                    <- this block's own trailing blank
# --- Fragment stages ---
```

**`<base>` is emitted as the literal four-character placeholder, not
interpolated.** Two reasons. It keeps the emitted comment byte-stable
regardless of manifest content, which is testable. And interpolation would be
*wrong* under `--pin-digests`: the user would need to inspect the digest-pinned
ref to describe the image actually being built, so substituting `manifest.base`
would hand them a command that inspects a different image than the one in the
`FROM` line.

**Why a global `ARG` at all, stated correctly.** It is not a mechanical
requirement. A `--build-arg`-supplied value reaches any stage that declares
`ARG NAME`; the stage-level `ARG CHUNKAH_CONFIG_STR` inside the chunker stage is
sufficient on its own. A pre-`FROM` `ARG` is required only to use a value in
`FROM` lines or to carry a *default* into stages, and there is no default here.
The global declaration is kept for two other reasons: it matches upstream's
example verbatim — the same diffability argument made for the mount options
below — and it documents the knob at the top of the file where a reader looks
for it.

This correction matters for the test plan. The index assertion on the global
`ARG` pins *upstream-matching shape*, not a silent breakage, and a maintainer
who later learns the real semantics must not delete it as cargo cult. **The
in-stage re-declaration is the one that is mechanically load-bearing**, and it
is the one whose absence is silent: without it, `--build-arg
CHUNKAH_CONFIG_STR=…` is accepted, the build succeeds, and the value never
reaches `chunkah build`. See Tests.

**2. The main stage is named.** `FROM <base_ref>` becomes
`FROM <base_ref> AS assembled`, only when `--rechunk` is set. `<base_ref>` is
computed exactly as today, digest-substituted under `--pin-digests`.

**No collision is possible for well-formed fragment names:** fragment stages are
emitted as `frag-<name>`, so a fragment cannot claim `assembled` or `chunkah`
through its name alone. Fragment names are not validated as identifiers today,
which is acceptable because a fragment already executes arbitrary code as root
at build time by design. Recorded because `--rechunk` makes stage-name integrity
load-bearing in a way it was not before: `--mount=from=assembled` selects the
rootfs that becomes the final image, making `assembled` the first privileged
stage name in the emission. If the fragment trust boundary ever tightens, this
is one of the places that assumes it.

**3. `RUN bootc container lint` does not move.** It is already the last
instruction of the main stage, which is already where upstream's bootc example
puts it — before the chunker consumes the rootfs. With `--rechunk` that stage
becomes `assembled` and the lint line stays exactly where it is.

No second lint is added against the chunked output. Upstream does not, the
rootfs content is byte-identical (chunkah's stated property is that content is
never modified apart from mtime), and — now decisive — **any instruction after
the final `FROM` costs the layer annotations**, which the emission below is
specifically shaped to keep. A second lint would also not be an integrity
control: it cannot detect a compromised chunker, and nobody should re-add it
citing the trust assumptions recorded below.

**4. The rechunk phase**, appended after the validation phase:

```dockerfile
                                    <- leading blank, emitted by this block
# --- Rechunk ---
# Requires podman >= 6.0.0 / buildah >= 1.44.0 on the machine that runs the
# build. With podman machine that is the VM, not the client: check the Server
# line of `podman version`. Older builders do not recognize FROM --after= and
# fail with an error like:
#   FROM only supports the --platform flag
FROM quay.io/coreos/chunkah:v0.6.0@sha256:ff8b8b466a942ec6000445d4001fc661e2fc5a952ad9ee29b4de9ab09d1d1708 AS chunkah
ARG CHUNKAH_CONFIG_STR
RUN --mount=from=assembled,src=/,target=/chunkah,ro \
    --mount=type=bind,target=/run/src,rw \
      chunkah build \
        --prune /sysroot/ \
        --max-layers 128 \
        --label containers.bootc=1 \
        --output oci:/run/src/rechunk.oci

FROM --after=chunkah oci:rechunk.oci
```

**Nothing follows the final `FROM`. That is a requirement, not an accident** —
see Trade-offs. The final stage is the whole output.

**The separating blank line is emitted by the rechunk block as a leading
separator, conditional on the flag.** The validation phase's emission is not
modified. This is what makes the "byte-identical when off" claim true: the
file's convention is that each section emits its own *trailing* blank, and an
implementer following that pattern would make validation's trailing blank
unconditional and thereby append a newline to every flag-off output.

Section-header style follows the file's existing `# --- Name ---` convention.
Two existing blocks depart from it — the validation phase's
`# --- Phase: validation (90) ---`, a survivor of the dropped `phase` field, and
the preset-apply phase's bare `# Apply systemd presets from fragments`. This
spec touches neither.

**The floor comment is emitted; the consumer-side caveats are not.** The
generated Containerfile is a handoff artifact, and the person who builds it is
the person the builder floor bites. The people the consumer floor and the
SELinux caveat bite are deploying the resulting image and may never see this
file, so those go to `--help` and the README instead. Each caveat where its
audience is. This is a decision, not an omission — do not add the consumer floor
to the generated output later on the theory that it was overlooked.

### On the chunker mounts' option set

The two chunker mounts depart from this file's house style on three axes, all
for one reason: they are copied verbatim from the upstream-verified pattern and
should stay diffable against it.

| Axis | House style | Chunker mounts | Upstream |
|---|---|---|---|
| `src=` vs `source=` | `source=` | `src=` | `src=` |
| SELinux relabel | `z` on every mount | omitted | omitted |
| Mount type | `type=bind` spelled | omitted on the rootfs mount | omitted |

`src` is a documented alias of `source`, and the rootfs mount relies on the
default mount type. The `z` omission is the one worth a caveat: it is
upstream-faithful, but no verification run has exercised an enforcing SELinux
host — every run was inside a privileged container using `--isolation chroot`.
chunkah's README does discuss `:z` cost on a large rootfs, but in the context of
the pre-1.44 `-v` workaround rather than `RUN --mount`.

All three are recorded together so a later consistency pass does not "fix" the
two that were previously unexplained into a divergence from upstream.

The `ro` on the rootfs mount is not stylistic and must not drift: losing it
would let the chunker mutate the assembled rootfs.

### The byproduct directory is named `rechunk.oci`

Decided 2026-08-03. Upstream's examples write `out`, and this spec deliberately
does not.

**`out` is an example argument, not a protocol name.** An OCI layout is
identified by its contents — `oci-layout` and `index.json` — never by its
basename, and the verification that this pattern works covered the *mechanism*
(a relative `oci:` path resolved against the build context), which applies to
any relative path.

**Two reasons the distinctive name is better**, and the first is the one that
matters:

- **Collision.** The byproduct lands in whatever directory the user builds from.
  In `--self-contained` mode that is normally the tool's own output directory;
  in registry mode it is an arbitrary user directory, where a generic `out/` is
  at its worst — that is exactly the name a user's own build output already has.
  `rechunk.oci` collides with essentially nothing in either mode.
- **Self-describing on disk.** Someone who finds it a week later can tell what
  wrote it and why without reading the Containerfile.

**Deliberately visible, not dot-hidden.** The directory is an image-sized copy
of the rootfs. Something that large should not accumulate invisibly.

Because it is visible and large, `--rechunk`'s `--help` and the README flag list
say it exists: a build writes a `rechunk.oci/` OCI layout into the build
context, it is not cleaned up automatically, and it is safe to delete **after a
successful build** — at which point the final `FROM` has already committed the
layout into the local image store. The tool cannot remove it; the tool exited
long before the build ran. See the caching interaction under `--self-contained`
for why deleting it can require `--no-cache` on the next build.

**Three sites must agree on this path**, not two: the context mount's
`target=/run/src`, the write at `--output oci:/run/src/rechunk.oci`, and the
read at `FROM --after=chunkah oci:rechunk.oci`. They are one location expressed
from two sides of a bind mount, plus the mount that creates it. Change the mount
target alone and the write lands on a non-mounted path inside the container
while both `rechunk.oci` tokens still match — a passing test and a failing
build. All three derive from the same constants.

One consequence to know rather than act on: `oci:` names a *directory* while the
`.oci`/`.ociarchive` suffixes elsewhere in this ecosystem usually mark archive
*files*. The name is still the right call; it just is not evidence about the
transport.

### Named constants

Five constants in the generator, no inline literals: the fully-qualified chunker
reference (repo, tag, and digest as one string), the context mount target, the
byproduct basename, the layer cap, and the prune path. The last two are the ones
whose raw values hide their reasons — `128` is a consumer bootc floor and
`/sysroot/` is an ostree implementation detail of current bootc bases — so the
names are where those reasons live.

**The chunker reference is a single constant carrying `repo:tag@digest`**, not a
repo plus a tag to be recombined. `repo:tag@digest` is valid reference grammar
that both podman and buildah accept; the digest is authoritative and the tag
keeps the emitted file readable. Holding it as one string means nothing needs
`split_image_ref` and nothing recombines it.

The pin is a maintenance surface and bumping it is a deliberate, tested change:
chunkah is 0.x, shipped nine releases in four months, and changed its own
recommended Containerfile form one release ago (`--output oci:` arrived in
v0.6.0, 2026-06-08, and the release notes encourage switching to it). Emitting
`:latest` would silently change behavior on the next release and is never
correct here. Bumping now touches two values in one constant instead of one.

**The digest value and how it was obtained.** `v0.6.0` resolves to
`sha256:ff8b8b466a942ec6000445d4001fc661e2fc5a952ad9ee29b4de9ab09d1d1708`
(2026-08-03). This is the **OCI image index** digest — `mediaType:
application/vnd.oci.image.index.v1+json`, with `linux/amd64` and `linux/arm64`
instances — not a per-architecture child manifest, so hardcoding it is
multi-arch safe and the generated Containerfile still builds on any
architecture. That was the obvious objection to pinning a tool image into a
handoff artifact, and the answer is favorable.

The value is corroborated three ways: the registry's `docker-content-digest`
header, an independent `skopeo inspect --override-os linux --format
'{{.Digest}}'` during review, and a local sha256 of the fetched index body
matching both. The last is what makes the value trustworthy independently of the
channel it arrived over — content addressing means a tampered body cannot hash
to this digest. **The plan should still re-resolve it before hardcoding** and
confirm it is unchanged; a constant that pins the wrong artifact is worse than a
tag.

## Mechanism, stated once

`FROM --after=chunkah oci:rechunk.oci` is not a stylistic choice.
`oci:rechunk.oci` is a local transport resolved relative to the build context,
and buildah cannot infer that an earlier stage produces it. Without `--after`,
the pattern requires the *builder* to be invoked with
`--skip-unused-stages=false` — a CLI flag a Containerfile cannot carry, which
would make this feature depend on out-of-band instructions to the user.
`--after` declares the dependency inside the file, which is what makes the
feature emittable at all.

Three properties were measured rather than inferred, by local build on buildah
1.44.0 (aarch64, Fedora rawhide, `buildah bud --no-cache`, 2026-08-03): an
`--after`-referenced stage survives unused-stage pruning with no extra build
flags (a genuinely unreferenced stage in the same file was pruned in the same
run, as a control); the same holds when the final `FROM` is the local-transport
form, resolved against the build context; and the ordering guarantee holds, with
the producer stage completing before the final `FROM` is evaluated. The runs
used `oci:out`; what they established is that a relative `oci:` path resolves
against the build context, which is a property of the transport and not of the
basename.

**Worth knowing about the posture this creates:** buildah's own documentation
describes `--after` as an ordering guarantee and does **not** document the
pruning exemption. The behavior is what it is on 1.44.0 and it was measured, but
"verified, not documented" is a weaker contract than "documented," and an
upstream change here would not be a regression against anything written down.

The final stage is not derived from the assembled image — it *is* the
chunkah-produced image. That is the mechanism by which the chunked output
becomes the build's result, and it is also why no check downstream of the
chunker can see what the chunker did.

`chunkah build` writes an OCI directory layout to `/run/src/rechunk.oci`, i.e.
`rechunk.oci/` inside the build context, in every mode.

## Recorded trade-offs

**The chunker stage has read-write access to the build context.**
`--mount=type=bind,target=/run/src,rw` gives the chunker image read-write access
to the build context directory for the duration of the build. This is the tool's
**first read-write context mount** — both existing bind mounts in the generator
carry `z` but no `rw`, so they are read-only under buildah's default. It is a
new category of emitted behavior, not a larger version of an existing one.

It is also structurally required and must not be narrowed: `FROM oci:` resolves
the layout relative to the build context root, so the layout has to land there,
and buildah cannot bind a subdirectory that does not yet exist.

The consequence, stated plainly because users deciding whether to flip an opt-in
flag are entitled to the accurate blast radius: **the chunker image is trusted
with the contents of the build context, not only with the image it produces.**
Under `--self-contained` that directory is the tool's own output, described as a
handoff artifact, committed to git and packaged into a sibling tarball —
including the `.osfragment-assemble` sentinel, an ordinary writable file in the
same directory. In registry mode it is whatever directory the user runs
`podman build` from, commonly a source repo root. This goes in `--help`
alongside the `rechunk.oci` note.

**The chunker image is pinned by digest by default, and that is deliberate.**
The tool, not the user, chooses this reference — `quay.io/coreos/chunkah`
appears in no user-authored file. Every other image in the emission is a ref the
user wrote in their own manifest and can see, evaluate, and pin themselves.
Making digest integrity for a tool-injected third-party ref contingent on
`--pin-digests`, a flag about the *user's own composition*, would be the wrong
coupling: an attacker who re-pushed the `v0.6.0` tag would control the binary
that produces the final image, with no gate after it — the final stage *is* the
chunker's output, and every existing check (`bootc container lint`, the
classifier, validation) runs upstream of the substitution and sees nothing. The
same mutability breaks reproducibility with no attacker at all.

**SELinux labels are not carried in the chunked layers.** chunkah does not
currently write `security.selinux` xattrs (chunkah#159, open 2026-07-27); the
resulting image relies on bootc relabeling on the client. Systems consuming a
`--rechunk` image must run a bootc that does so. This is the status quo for
chunked images generally and is not introduced by this tool, but it is a
property of turning the flag on, and it is the one caveat here whose failure is
quiet rather than loud — a version floor produces exit 125, missing MAC labels
degrade enforcement on a deployed fleet. **Documented in `--help` for
`--rechunk` and the README flag list, alongside the consumer bootc floor**,
which it reinforces and where it costs no new documentation surface.

**Full label preservation via `CHUNKAH_CONFIG_STR` is declined as the default.**
`--build-arg CHUNKAH_CONFIG_STR="$(podman inspect $BASE)"` preserves every base
label including versioning metadata. Emitting a Containerfile that requires it
would mean the tool either runs `podman inspect` itself — crossing the line the
project exists to refuse — or ships a file that only builds when the caller
remembers a precomputed argument. The declared `ARG` is the escape hatch: a user
who wants full metadata supplies it and nothing in the file changes.

Two consequences worth knowing if that hatch is used: `chunkah build` then wants
`--label ostree.commit- --label ostree.final-diffid-` to strip labels carried in
by the config, and a large config string can exceed the environment size limit
(chunkah#136, closed 2026-06-01, with `--config` reading from a file as the
escape). Neither is emitted by default, because with no config supplied there
are no such labels to strip and those arguments are no-ops. Both belong in
`--help` rather than in unconditional emission.

### Reopened and resolved: the annotation loss was avoidable

An earlier revision of this spec recorded, under "settled, do not reopen," that
`--rechunk` accepts the loss of chunkah's informational layer annotations
(containers/buildah#6652, open since 2026-01-23) as the price of re-applying the
required `containers.bootc=1` label with a post-`FROM` `LABEL` instruction. It
called that "the correct trade."

**It was not a forced trade, and the item is withdrawn.** chunkah's README
states the loss condition precisely: annotations fail to persist "when
additional instructions follow the final `FROM`." The `LABEL` was that
instruction, and it was the only one. chunkah can set the label itself —
`chunkah build --label containers.bootc=1` — and `--label` does **not** require
`--config-str`: verified against `src/cmd_build.rs` at tag `v0.6.0`, where
`parse_key_value_pairs` starts from the config's labels (an empty map when no
config is supplied) and applies CLI pairs on top. It is idempotent if the escape
hatch is also used.

So the emission keeps **both** the required label and the annotations, and
nothing follows the final `FROM`. This is why the "no second lint" rule above
now has a second and stronger reason behind it.

Recorded at this length for two reasons. The "settled, do not reopen" list is
where a future maintainer is told not to re-litigate, so an avoidable loss
recorded there as forced would have kept the annotations gone for the life of
the feature — and it would have propagated a false rule, that the label must be
applied post-`FROM`, into the tests. And the escape clause that let it be
reopened is worth naming: the list stops re-litigation of decisions, it does not
stop a must-fix finding that the recorded reasoning was factually wrong.

**One honest gap.** No verification run has built a final stage with **zero**
instructions after `FROM`; the local runs all had a `RUN` there. It is ordinary
Containerfile shape and no issue is expected, but it is now load-bearing for the
annotation outcome, so it is on the manual end-to-end list rather than assumed.

## Flag interactions

### `--pin-digests`

**No interaction.** The chunker reference is already digest-pinned by the
constant, in every mode, so `--pin-digests` has nothing to do for it. This
removes the extra registry round-trip an earlier revision accepted, and it keeps
`generate_containerfile` free of I/O — every network call stays in
`main.rs`/`loader.rs`, and the generator continues to receive resolved values as
plain data.

The chunker digest does **not** appear in the header's `# Resolved digests:`
block, in any mode. That block reports digests the tool resolved at generation
time; the chunker's is a compile-time constant, and resolving nothing produces
nothing to report. The block is unchanged by this flag.

This also settles what an earlier revision left undefined: there is no
`--pin-digests --self-contained --rechunk` question, because there is no
unpinned chunker mode for the three-way combination to disagree about. The
self-contained suppression rule is about *fragment* refs; the chunker is not
one, and it is emitted identically in all three modes.

### `--self-contained`

**Permitted.** A chunker image pull is one more build-time registry pull, in the
same category as the base image pull that already exists in this mode. Refusing
the combination would be over-restrictive and inconsistent with the base-image
precedent.

**Wrinkle 1 — `oci:rechunk.oci` is excluded from context-path validation by
accident, not by rule.** `context_paths_referenced` — a **test helper** inside
`#[cfg(test)] mod tests`, not production code — collects build-context paths by
taking every whitespace/comma-delimited token, stripping an optional `source=`
prefix, and keeping those that begin `fragments/`. `oci:rechunk.oci` does not
match that prefix and is silently ignored. That outcome is correct — the layout
is produced by the build, not by materialization, so demanding it on disk would
be wrong — but it holds only because of the prefix filter. The hook-command scan
in the same test keys on `source=fragments/` and skips the chunker's mounts for
the same accidental reason: neither chunker mount carries a `source=` at all.

**Requirement:** when a `--rechunk` case is added to
`emitted_containerfile_paths_resolve_in_the_materialized_tree`, the exclusion
must be made explicit — at minimum a comment at the filter stating that
build-produced paths are deliberately out of scope. Otherwise a later
generalization of the helper (say, "collect every non-absolute path token")
starts demanding `rechunk.oci/` on disk and fails for a reason nobody will
recognize. The plan should know it is editing a test file.

**Wrinkle 2 — a rechunk build normally leaves `rechunk.oci/` in the output
directory, and the sentinel guard then refuses to regenerate into it.** In the
expected invocation the build context is `<dir>`, so
`chunkah build --output oci:/run/src/rechunk.oci` writes `<dir>/rechunk.oci`. (A
user who builds with `-f <dir>/Containerfile <other-context>` puts it elsewhere
and never trips this; the expected invocation is not a guarantee.)
`TOOL_GENERATED_ENTRIES` is exactly `Containerfile`, `manifest.yaml`,
`fragments`, `.osfragment-assemble`, and `check_target_dir_safe` requires
*every* entry in the directory to be recognized. So the next
`osfragment-assemble --self-contained <dir> --rechunk` refuses, on the obvious
build-then-regenerate loop, with a message that names nothing about the cause.

**Resolution, decided 2026-08-03: extend the error message, change no
behavior.** `check_target_dir_safe` refuses in exactly the cases it refuses in
today, and its existing message text is preserved **verbatim**. When
`rechunk.oci` is among the unrecognized entries, an additional sentence is
appended:

```
<existing message, verbatim and unchanged>. Note: this directory contains
rechunk.oci, which is the name a --rechunk build writes its OCI layout to in
the build context; if that is what this is, remove it and re-run.
```

and when other unrecognized entries are present alongside it, the appended
sentence names them too, so the user is not sent to do a partial fix and refused
again:

```
… ; if that is what this is, removing it will not be enough on its own — this
directory also contains: <other entries>.
```

Three properties of that wording are deliberate. It is **additive**, so the
non-`rechunk.oci` message is byte-identical to today's. It **describes what
writes that name** rather than asserting what the entry is —
`check_target_dir_safe` reads `file_name()` and never stats, so the entry could
be a user's own file or directory, and the guard's stated virtue is
content-blindness. And it **does not promise that one `rm` is sufficient** when
it is not.

**Rejected: adding `rechunk.oci` to `TOOL_GENERATED_ENTRIES`.** That constant
means "entries the tool itself may have written," and this is not one — the
builder wrote it, on a later and separate run. Since `write_output_with` calls
`fs::remove_dir_all(dir)`, adding it would make the tool silently delete an
image-sized directory on *every* regeneration, not just rechunk ones, widening a
deliberately narrow guard to cover something outside it. A loud refusal the user
resolves with one `rm` is the better failure.

**Layer caching, and why the documented remedy can bite.** Every verification run
used `buildah bud --no-cache`, the least cache-exposed configuration available.
The tool's users run `podman build`, where `--layers` defaults to **true**
(buildah's own default is false; the defaults differ). Verified configuration
and default user configuration differ on exactly this axis.

The reassuring part, traced in buildah's `imagebuildah/stage_executor.go`: a
`RUN --mount` sourcing from a stage with `DidExecute && IsStage` sets
`avoidLookingCache` and skips cache matching for that instruction entirely. So
whenever `assembled` re-executes, chunkah re-runs. That also covers the
otherwise-worrying containers/buildah#6957 (merged 2026-07-14, shipped v1.45.0),
which fixed `RUN --mount` flags omitting `type=` not being checksummed for cache
invalidation — the shape of this emission's first chunker mount, on a floor that
predates the fix.

The residual hole is reachable on the documented happy path: if `assembled` is
fully cache-hit because nothing in the composition changed, `DidExecute` is
false, the chunker `RUN` becomes cache-eligible, and chunkah does not run. If
`rechunk.oci` was deleted in the meantime — which `--help` calls safe, and which
the new guard message explicitly *instructs* — the final `FROM` fails on a
missing layout, on a build that succeeded yesterday with no source change. The
loop documented as the remedy is the loop that reproduces this.

The failure is loud, so this is a documentation fix rather than a design change:
`--help` and the README note that deleting `rechunk.oci` may require
`--no-cache` (or `--layers=false`) on the next otherwise-unchanged build.

### Documentation wording

The claim that a self-contained build needs no registry except the base image
appears in four places and becomes conditionally false under `--rechunk`. It is
corrected in three of them:

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
line is aimed at.

## Documented floors

These feed `--help` for `--rechunk` and the README's flag list. **The tool never
probes the user's builder version.** It generates; it does not wrap, and it does
not interrogate the environment it generates for. These are documented
requirements, not runtime checks.

### Builder: podman >= 6.0.0 / buildah >= 1.44.0, on the machine that runs the build

`FROM --after=` shipped in buildah v1.44.0 (2026-05-27). Verified from `go.mod`
at each podman release tag (2026-08-03): podman v6.0.0 (2026-06-24) is the first
release to vendor it. Every podman 5.8.x — including v5.8.5 (2026-07-08), which
is *newer* than v6.0.0 — still vendors buildah 1.43.2 and cannot build this
pattern. "Update podman" is therefore not sufficient advice; the requirement is
the 6.x line.

**The version that matters is the server's, not the client's, and on macOS and
Windows they are different programs.** `podman build` ships the build to the
`podman machine` VM, and the buildah that must be >= 1.44.0 is the one vendored
by the VM's podman. This is not hypothetical: the verification run recorded
client 5.8.3 against server 5.8.4 on the same machine. A user who upgrades the
pkginstaller to 6.0.2, runs `podman --version`, sees 6.0.2, and does not update
or recreate the machine image gets `FROM only supports the --platform flag` from
a podman that reports itself as above the floor.

So the documented floor must say **which** version to check:

> Requires podman >= 6.0.0 (or buildah >= 1.44.0) on the machine that runs the
> build. With `podman machine` — the default on macOS and Windows — that is the
> VM, not the client: check the **Server** line of `podman version`, and
> recreate the machine if it is below 6.0.0.

The whole "documentation problem, not a correctness trap" argument rests on this
being actionable. Failure on an older builder is loud and unmistakable —
measured against podman 5.8.4, buildah parses the file, builds the first stage,
and errors on the final `FROM`, exit 125, with **no silent fallback to an
unchunked image**. That is the opposite of the OCL asymmetry driving the `--ocp`
refusal. But loud only helps if the message sends the user somewhere, and a
floor that does not distinguish client from server sends them nowhere.

Worth documenting for anyone reproducing this: no official container image could
build the pattern as of 2026-08-03 — `quay.io/buildah/stable` was at v1.43.1 and
`quay.io/podman/stable` at v5.8.2, both below the floor (re-confirmed via the
Quay tag API during review).

### Consumer: bootc >= 1.1.3 on every system that pulls the resulting image

This is the rationale for `--max-layers 128` and not a larger value: chunkah
documents a minimum bootc of v1.1.3 for <= 128 layers and v1.9.0 for more, so
128 keeps the fleet floor four minor versions lower. Upstream also recommends
raising the default from 64, so 128 is both the recommended value and the one
with the cheaper deployment requirement. This is a fleet-wide constraint — users
need to know it before flipping the flag, not after. The SELinux caveat under
Trade-offs is documented in the same place, for the same audience.

### What is measured and what is inferred

The floors rest on a mix of both, and the distinction is maintained
deliberately. This table is the single place it lives, and it is written to
absorb a measured result later by changing a row's status rather than by
reworking prose elsewhere.

| Claim | Status |
|---|---|
| `--after` exempts a stage from unused-stage pruning | **Measured** — buildah 1.44.0, aarch64, `buildah bud --no-cache`, with a pruning control in the same run |
| The exemption holds for a local-transport `oci:` final `FROM`, resolved against the build context | **Measured** — same conditions |
| `--after`'s ordering guarantee holds (producer completes before the final `FROM` is evaluated) | **Measured** — same conditions |
| podman 5.8.4 rejects `--after` loudly, exit 125, with the quoted message | **Measured** — one data point, from `openshift/imagebuilder` v1.2.19's `FROM` parser. The emitted comment says "an error like" rather than quoting it as universal |
| podman >= 6.0.0 vendors buildah >= 1.44.0; 5.8.x does not | **Measured** — `go.mod` at each release tag |
| The chunker digest is the multi-arch index, not an arch-specific child | **Measured** — index `mediaType`, two platform entries, and a local sha256 of the index body matching the registry's `docker-content-digest` |
| buildah 1.44.1 (what podman 6.0.2 actually vendors) behaves as 1.44.0 does here | **Inferred** — not run |
| x86_64 behaves as aarch64 does here | **Inferred** — not run |
| `podman build` as the front end behaves as `buildah bud` does | **Inferred** — all runs used `buildah bud` directly. Podman's `build` is a thin wrapper over the same vendored executor, so no difference is expected, but none was observed. This is the configuration nearly every user will actually be in |
| `--after` also removes upstream's documented "cannot use `--jobs`" restriction | **Inferred** — containers/buildah#6621, the issue that produced `--after`, was opened because the trick "breaks with `--jobs N` for N != 1", and `--after`'s documented semantic is the wait this needs. No run used `--jobs`. `--jobs` is not exotic in CI, and the failure if the inference is wrong is loud: a missing layout at the final `FROM` |
| Layer caching does not break the pattern | **Inferred from source, not run** — every run used `--no-cache`, while `podman build` defaults `--layers=true`. Reasoning and the residual hole are under `--self-contained` above |
| A final stage with **zero** instructions after `FROM` behaves normally | **Inferred** — ordinary Containerfile shape, but every run had a `RUN` there, and this is now load-bearing for the annotation outcome |
| OCP build pods do not ship buildah >= 1.44.0 | **Inferred** — from OCL running the RHEL 9 buildah RPM plus general lag. The `--ocp` refusal message is worded accordingly |

Every **Inferred** row is on the manual end-to-end verification list below.

## Tests

Generator unit tests, mirroring the shape of the existing lint suite
(`container_base_omits_bootc_steps`, `bootc_base_preserves_both_steps`): build a
manifest and fragments, call `generate_containerfile`, assert on the emitted
string. Whole-line comparison where a line is asserted, following the existing
`hook_invocation_lines` helper, which compares whole lines precisely because a
partial match would let a differently indented or extended command through.

**Emission, flag on, bootc base:**

- **The chunker stage line**, whole-line:
  `FROM quay.io/coreos/chunkah:v0.6.0@sha256:… AS chunkah`. Digest and tag are
  both present — this is the combined `repo:tag@digest` form, and asserting only
  the digest would pass on a regression that dropped the tag.
- **Both mount lines, whole-line.** They are the mechanism: the first is how
  chunkah gets a rootfs to chunk, the second is how its output escapes the
  container. Asserting `--prune /sysroot/` while ignoring the mount that
  supplies the thing being pruned inverts the priorities. `ro` on the rootfs
  mount is covered by the whole-line comparison — losing it would let the
  chunker mutate the assembled rootfs.
- **The `assembled` stage name agrees at both sites.** It is emitted at
  `FROM <base_ref> AS assembled` and consumed at `--mount=from=assembled`. Same
  one-name-two-sites coupling as the byproduct path, and it needs the same
  paired assertion. A wrong stage name is not rejected as an unknown stage —
  `from=` accepts a registry image ref too — so it surfaces as a confusing
  registry pull failure rather than a clear error.
- **The byproduct path agrees at all three sites.** Extract the value from the
  context mount's `target=`, from the `--output oci:` token, and from the
  `FROM --after=` token; assert the mount target is a prefix of the output path
  and the two `rechunk.oci` basenames are equal. Derive the comparison from the
  emitted text rather than asserting three hardcoded literals: a literal-triple
  assertion pins only that the sites *currently agree*, which is worth having,
  while a derived-equality assertion also survives a deliberate rename and still
  catches divergence.
- `--prune /sysroot/`, `--max-layers 128`, and `--label containers.bootc=1` are
  present in the `chunkah build` invocation.

**Absence and structure, flag on:**

- **Nothing follows the final `FROM`.** The `FROM --after=chunkah oci:rechunk.oci`
  line is the last non-empty line of the output. This is the assertion that
  protects the layer annotations, and it is what would catch a well-meaning
  re-addition of `LABEL containers.bootc=1` or of a second lint. An absence
  assertion, not a presence one — that is the point.
- **No `LABEL containers.bootc=1` instruction appears anywhere in the output.**
  The label now travels as a `chunkah build --label` argument. Both assertions
  are needed: the first catches anything appended after the final `FROM`, the
  second catches the specific regression of moving the label back.
- **Lint stays in the assembled stage.** `RUN bootc container lint` appears
  *after* the `AS assembled` line and *before* the chunker `FROM`. A `contains`
  assertion alone is not sufficient — it passes just as happily if the lint
  migrates, which is the regression worth pinning.
- **The build-arg escape hatch is wired end to end.** `ARG CHUNKAH_CONFIG_STR`
  appears exactly **twice**: once before the first `FROM`, and once between the
  chunker `FROM` line and the `RUN … chunkah build` line. Assert both indices
  and the count; a `contains` assertion cannot distinguish the two occurrences.
  Dropping the in-stage one is the single worst failure mode in the feature: the
  file builds, `--build-arg CHUNKAH_CONFIG_STR=…` is accepted, and the value is
  silently ignored, producing exactly the label-stripped image the user was
  trying to avoid — no error, no warning, every other test still green.
- **The emitted comments are present and byte-stable.** The builder-floor
  comment and the `ARG` explanatory comment are both asserted; the spec argues
  both are non-optional in the same terms and both should be pinned. Assert the
  `ARG` comment is byte-identical across two manifests with different base refs,
  which is what pins `<base>` as a literal placeholder rather than an
  interpolation.

**Flag off:**

- **A default-mode golden.** `assert_eq!` on the full output, with pinned
  fragments so the `# --- Fragment stages ---` block is present, mirroring the
  existing `self_contained_output_matches_golden_containerfile`. The spec claims
  byte identity when the flag is off; a handful of `!contains` checks do not
  verify byte identity and a golden does. Default mode is also where the rechunk
  emission is most structurally intrusive — the global `ARG` lands ahead of the
  fragment stages and the base line gains `AS assembled` — and it is the one
  mode with no golden today.

**Refusals:**

- **`--ocp` plus flag.** Extract as
  `fn check_rechunk_ocp_conflict(rechunk: bool, ocp: Option<&Path>) -> anyhow::Result<()>`.
  A `bool`-returning predicate in the style of `should_keep_fragment_digests`
  cannot carry a message, which would leave the message untested at the call
  site — the defect class this test plan exists to avoid. Assert both directions
  and the message text. Note the predicate tests `ocp.is_some()`: `--ocp` is
  `Option<PathBuf>` with `num_args = 0..=1`, so flag presence is not a bool.
- **Declared-container base plus flag.** Same shape, keyed on
  `manifest.base_type`, asserting the message names the flag and `baseType`.
- **Precedence.** Both conditions together produce the `--ocp` message.
- **The generator's contract check**, three capability sets: full bootc (emits),
  empty (errors), and **Systemd-only** (errors). The third is what distinguishes
  the correct predicate from `is_empty()`, and the suite already constructs that
  set elsewhere.

**Interactions:**

- **`--pin-digests` changes nothing about the chunker.** Generate the same
  manifest with and without `--pin-digests` and assert the chunker `FROM` line
  is identical in both. Stronger than asserting the digest is present, and it is
  the actual claim: the constant is the pin.
- **No chunker digest in `# Resolved digests:`.** Assert the block does not
  mention chunkah, in both default and self-contained modes.
- **`--self-contained` plus flag.** The chunk phase is emitted, and
  `oci:rechunk.oci` is not collected by `context_paths_referenced`. This is what
  makes the exclusion deliberate rather than incidental.
- **The guard message branches; the guard does not.** Three scenarios:
  (a) sentinel plus tool entries plus `rechunk.oci` → refused, message contains
  the appended sentence naming `rechunk.oci`;
  (b) **control** — sentinel plus tool entries plus some *other* foreign entry →
  refused, message **verbatim equal** to today's text and containing no mention
  of `rechunk.oci`;
  (c) both → refused, message names the other entries too.
  The control must be a verbatim equality assertion. The repo's four existing
  refusal tests all assert
  `err.to_string().contains("was not generated by this tool")`, a substring both
  branches share — written in that style the control passes identically whether
  the message branched correctly, branched wrongly, or did not branch at all.
  This is the defect class the entrypoint review caught as SF-1 and closed with
  verbatim error-message assertions; the precedent is in this repo.

**Live end-to-end build is manual-only for now.** Verifying that the emitted file
produces a chunked bootc image requires podman 6.x, which the project's own
development machine does not have — every podman 5.8.x hard-errors on
`FROM --after=`. The unit tests pin the emitted *text*; the `--after` mechanism
underneath it was measured separately (see Mechanism). The manual run should
close every **Inferred** row in the table above, and in particular check the two
faults most likely to be found by it and least likely to be found by anything
else: a dropped in-stage `ARG`, and a wrong mount line.

## CHANGELOG

Under `### Added`, non-breaking: `--rechunk` emits a chunkah rechunk phase in
the generated Containerfile; the assembled stage is named `assembled`, chunkah
applies `containers.bootc=1` itself, and the chunked output becomes the final
image. Note the builder floor (podman >= 6.0.0 / buildah >= 1.44.0, **server
side** under `podman machine`), the consumer floor (bootc >= 1.1.3), the SELinux
relabeling dependency, the refusals with `--ocp` and with a declared-container
base, that under `--self-contained` a build now also pulls the chunkah image,
and that a build writes a `rechunk.oci/` OCI layout into the build context which
is not cleaned up automatically.

Under `### Changed`, if the guard message lands as its own change:
`check_target_dir_safe` now names `rechunk.oci/` as a `--rechunk` build
byproduct when it finds one, appended to its existing text. No change to which
directories it accepts.

Nothing is breaking: with the flag off, emission is byte-identical to today.
Shipped examples and generated documentation samples need no regeneration.

## Out of scope, recorded

- **`--ocp` support.** Refused above. Revisit when OCP ships buildah >= 1.44.0.
- **Rechunking only layers added since `FROM`** (chunkah#127, opened 2026-05-07,
  still open). Almost certainly the *better* mode for this tool — it would chunk
  only fragment-added content and leave base layers intact, preserving
  cross-image base reuse. It would change the emitted arguments. **This is the
  watch item.** Adopting it later is cheap under the project's
  no-backwards-compatibility policy: change the constants and the argument list,
  rebuild the examples. Do not design around a feature that does not exist.
- **Full label preservation by default.** Declined above; the `ARG` is the
  escape hatch.
- **Probing the user's builder version.** Never. The tool generates; it does not
  interrogate the environment its output will run in. Documented floors are the
  whole mechanism.
- **A `--rechunk-max-layers` knob.** 128 is upstream's recommendation and carries
  the lower consumer floor. Wait for someone to need otherwise.
- **cosign verification of the chunker image.** chunkah images are keyless-signed
  and a real supply-chain story exists, but it is a separate feature with its own
  surface. Digest pinning is the substantive control for v1 and it is present by
  default.
- **Narrowing the read-write context mount.** Structurally impossible; see
  Trade-offs. Do not attempt it.
- **Fragment name validation.** Outside this flag's delta. Noted under Emitted
  structure item 2 as an assumption `--rechunk` newly depends on, and tracked
  separately.
- **Fragment-declared layer grouping.** chunkah supports `user.component` and
  `user.update-interval` xattrs for grouping, and a fragment declaring its own
  update cadence so chunkah packs it sensibly is a genuinely interesting future
  fit between the two formats. Not v1.

## Open for the implementation plan

- **How the new emission input reaches the generator.** `generate_containerfile`
  already takes six positional parameters, two of them bare `bool`s (`ocp`,
  `self_contained`), and `--rechunk` adds one more flag. With the chunker digest
  now a constant, no resolved value needs threading, so the options are: one more
  positional parameter (pushing the signature to seven), or an options struct.
  Not decided here — it is a code-shape question and the behavior above is
  identical under both. Whichever is chosen, **the OCP call at `main.rs:202`
  passes `rechunk: false` explicitly, with a comment naming the refusal as the
  reason.** It is provably false there today, and passing it through implicitly
  would create a silent dependency on a refusal in distant code.

Settled, recorded here so the plan does not reopen them: the chunker image is
pinned to `v0.6.0` by tag **and digest** in one constant, never `:latest`;
`--max-layers` is 128 and the reason is the consumer bootc floor, not packing
quality; `bootc container lint` does not move and is not duplicated; **nothing
follows the final `FROM`, and `containers.bootc=1` travels as a `chunkah build
--label` argument**; the builder floor is emitted as a comment and the consumer
floor is not; both CLI refusals are errors rather than warnings and live in
`main.rs` after `parse_manifest`, with `--ocp` winning when both apply; the
generator's contract check is keyed on the `Capability::Bootc` predicate; the
byproduct directory is `rechunk.oci`, visible rather than dot-hidden; and
`check_target_dir_safe` keeps its current behavior and its current message text
exactly, gaining only an appended sentence.

The escape clause, since this list has now been reopened once: it stops
re-litigation of settled decisions, not a must-fix finding that the recorded
reasoning was factually wrong. The annotation trade-off above is what that looks
like.
