# Generator and Schema Changes

**Status:** Proposed (revised after round 1 review)
**Date:** 2026-07-27

Consolidates five changes to the generator, the OpenShift emitter, and the
fragment schema. Each is independent and can land separately. Change 1 fixes a
defect; the rest are design changes.

---

## 1. Build inputs stay out of the target image

**What.** Execute hooks via a bind mount instead of copying them in:

```dockerfile
RUN --mount=type=bind,from=<fragment>,source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z \
    /frag-hooks/10-configure.sh && /frag-hooks/20-enable.sh
```

`bind-propagation=rshared,z` is part of the emitted form, not decoration on the
example. It mirrors MCO's own on-cluster-build template; see "Two details to
carry into implementation" below for why, and the acceptance criteria for the
test that holds it in place.

All hooks belonging to one fragment run in a **single** `RUN --mount`
instruction, chained with `&&`, which is the chaining the current code already
produces. One instruction per fragment, not one per hook: the mount lifetime is
scoped to the instruction, so a per-hook instruction would remount for each hook
and add instructions to a Containerfile that is character-capped in OCP mode.

Each fragment's mount references its own image, so mount sources are never
shared across fragments and there is nothing to consolidate. A future
implementer should not try to collapse them into one mount.

Applies to hooks and any future non-`tree/` payload. `tree/` content keeps being
copied; that is the delivered payload, not a build input.

**Why.** The current emission is a defect. `generator.rs` writes the `COPY` and
the `RUN ... && rm -rf` as separate instructions, so they land in separate
layers. The `rm -rf` only writes a whiteout; the hook bytes remain in the
earlier layer and ship in the final image, recoverable with `podman save`. The
cleanup does not do what it was written to do.

The bind mount is one instruction and one layer, so nothing persists and no
cleanup is needed. It also removes a failure class: hooks can no longer collide
in `/tmp`, and nothing leaks when a hook exits nonzero mid-chain.

**Why there is no COPY fallback.** An earlier draft retained a flag-selected
COPY fallback for builders that might not support `RUN --mount`. That fallback
is removed. `RUN --mount=type=bind` is a BuildKit/Buildah extension, and buildah
has supported it since 1.24.0 (January 2022). Every target platform ships a
newer version. If a user hits a builder that rejects the instruction, the error
is immediate and legible; writing the COPY line by hand is a one-time
workaround, not a mode the tool needs to own.

The on-cluster OpenShift case warrants separate analysis because it is the
environment users most often assume `RUN --mount` cannot reach.
`RUN --mount=type=bind` takes a `from=`
with three distinct sources:

- `from=context`, which requires a build context holding the user's files.
- `from=<registry image ref>`, which the builder pulls.
- `from=<named build stage>`, which resolves inside the Containerfile.

This tool generates only the second and third forms. It never generates
`from=context`.

That distinction matters because the usual objection to on-cluster builds does
not apply to the forms emitted here. An on-cluster build does have a build
context: MCO hands the builder the Containerfile from the MachineOSConfig along
with a context directory. What it lacks is a build context **the user
controls**, so there is nowhere to place files for `from=context` to reach.

Registry resolution inside the build pod is likewise not the open part. The OCP
output already depends on it: `COPY --from=<registry ref>` is emitted for every
fragment carrying content, and with `--pin-digests` the output additionally
emits `FROM <fragment ref> AS frag-<name>` stages ahead of `FROM configs AS
final`. If registry pulls did not resolve in the build pod, the OCP path would
not work at all today.

What was genuinely unverified was narrower: whether the builder MCO runs accepts
the `RUN --mount` instruction at all, and whether the mount succeeds inside that
pod given its security context, mount permissions, and builder invocation.

**That question has now been answered: it works.** MCO's own on-cluster-build
Containerfile template already contains this exact instruction form, with
`from=` pointing at a registry image pullspec:

```dockerfile
RUN --mount=type=bind,from={{.ExtensionsImage}},source=/,target=/tmp/mco-extensions/os-extensions-content,bind-propagation=rshared,z \
    bash <<'EOF'
```

(`openshift/machine-config-operator`,
`pkg/controller/build/buildrequest/assets/Containerfile.on-cluster-build-template`.)
It is rendered into the same Containerfile that carries the user's
`containerFile` content, passed through the same validator, and executed by the
same `buildah bud` invocation in the same pod, on every extensions-enabled OCL
build. Buildah routes `COPY --from=<image>` and `RUN --mount=type=bind,from=<image>`
through the same image-rootfs resolution and the same `--authfile`, so the mount
form requires strictly nothing that the `COPY` form this tool already emits does
not already require.

**Consequence: the bind mount is the only hook emission path, for all output.**
There is no fallback mode, no flag to select one, and no target-driven
divergence. Every output path — standalone and MachineOSConfig — emits
`RUN --mount`.

This reverses the rule that round 1 review asked for (MachineOSConfig output
implies the fallback). That recommendation was correct given what was known at
the time; the verification removed its premise.

Two details to carry into implementation:

- **Mirror MCO's mount options** (`bind-propagation=rshared,z`) rather than
  emitting the bare form. MCO presumably added them for a reason and matching a
  production template costs nothing.
- MCO's production evidence covers `source=/`. This tool mounts a subdirectory
  (`source=/fragment/hooks`). Subdirectory sources are standard buildah with no
  special-casing, but the evidence is one step less direct there.

Caveats that do not gate implementation:

- **`source=<subdirectory>`.** The production evidence (MCO template) uses
  `source=/`. This tool mounts `source=/fragment/hooks`, a subdirectory.
  Subdirectory sources are standard buildah and require no special-casing, but
  the evidence is one step less direct.
- **`from=<stage>` vs `from=<registry pullspec>`.** Production evidence covers a
  registry pullspec. Under `--pin-digests` the emitted form resolves to a named
  build stage (`FROM <fragment ref> AS frag-<name>`). The conclusion holds — OCL
  builds already use named stages via `COPY --from=` — but the mount's `from=`
  uses a stage where the evidence uses a pullspec.
- **Mount `from=` resolution under `--pin-digests`.** The existing `COPY --from=`
  path pulls fragment content by digest-pinned ref. The mount's `from=` must
  resolve through the same mechanism. If it does not, an implementer could leave
  hook images pulled by mutable tag while payload images stay pinned. Specify
  that the mount `from=` uses the same resolution as `copy_from_source`.

**`--ocp` adds a second artifact.** `--ocp <path>` adds a MachineOSConfig
alongside the standalone Containerfile; it does not switch modes. Both artifacts
emit hooks via `RUN --mount`. The standalone Containerfile is still written by
the same invocation, and both carry the same hook emission.

**Why COPY was rejected (context for the bind-mount-only decision).**

The current two-instruction form (`COPY` then `RUN ... && rm -rf`) is
**filesystem-correct but not layer-correct**. The `rm -rf` writes a whiteout,
so the files are absent from the mounted filesystem and absent from what a
deployed bootc node sees. The bytes themselves remain in the `COPY` layer and
are extractable with `podman save` or `skopeo copy`.

No single-instruction fix exists. `COPY` is a build directive, not a shell
command; it cannot be combined with `rm -rf` in one instruction, and it always
produces its own layer. An earlier draft of this spec claimed the fallback could
fold the `rm -rf` into the `COPY` and become correct. That claim is withdrawn;
the operation it describes does not exist.

The alternatives considered:

- **Multi-stage variant.** Copy hooks into a disposable stage, then
  `RUN --mount=type=bind,from=<stage>` in the final stage. This eliminates the
  leak, but it depends on `RUN --mount`, so it is not a path that avoids the
  instruction — it is just a different use of the same instruction.
- **`--squash`.** Lossy, non-standard, and contrary to the hand-maintainable
  Containerfile goal. Rejected.
- **Accept the leak as a documented limitation.** Hook scripts are operational,
  not secrets, so the impact is pull size at fleet scale plus disclosure of a
  fragment's implementation logic (relevant for security-oriented fragments such
  as `cis-hardening`).

The bind mount avoids this entire class of problem: one instruction, one layer,
nothing committed, nothing to clean up. With the verification that it works on
every target platform including on-cluster OCP builds, there is no remaining
reason to carry a COPY path.

**Acceptance.**
- All output executes hooks via `RUN --mount`, with no `COPY` of `hooks/`.
  There is no fallback mode.
- A test asserts `hooks/` never appears in a `COPY` instruction in either
  standalone or `--ocp` output, and that the two paths do not diverge in hook
  emission.
- All hooks belonging to one fragment execute in a single `RUN --mount`
  instruction, preserving the current `&&` chaining.
- Emitted mounts carry `bind-propagation=rshared,z`, matching MCO's own
  on-cluster-build template.

---

## 2. MachineOSConfig v1 migration

**What.** Update `src/ocp.rs` from `machineconfiguration.openshift.io/v1alpha1`
to `v1`:

| Current output | v1 |
|---|---|
| `.../v1alpha1` | `.../v1` |
| `spec.buildInputs.{...}` / `spec.buildOutputs.{...}` | flat: `spec.containerFile`, `spec.imageBuilder`, `spec.renderedImagePushSpec` |
| `imageBuilderType: PodImageBuilder` | `Job` (the only enum value) |
| `spec.buildInputs.renderedImagePushspec` | `spec.renderedImagePushSpec` (casing change, `s` to `S`) |
| `spec.buildInputs.renderedImagePushSecret` | `spec.renderedImagePushSecret` (required field, path change only) |
| `spec.buildOutputs.currentImagePullSecret` | removed from spec |
| `containerfileArch: noarch` | `containerfileArch: NoArch` (casing change, see below) |

**Why.** The emitted YAML is rejected by current clusters. The `v1alpha1` type no
longer exists upstream, and `PodImageBuilder` is not a valid builder type.

Two current behaviours are correct and must be preserved: the 4096-character
limit on `containerFile.content` is real and enforced, and setting
`metadata.name == spec.machineConfigPool.name` satisfies a CEL validation rule
that would otherwise reject the object.

### `containerFile` shape and the `NoArch` casing

`containerFile` in v1 is a list keyed by architecture:

```yaml
spec:
  containerFile:
    - containerfileArch: NoArch
      content: |
        ...
```

Constraints on the field: `MaxItems=4`, `MinItems=0` (the field is optional, so
a MachineOSConfig without container file content is valid; this tool always has
content and always emits it), `listType=map` keyed on `containerfileArch`, and
`MaxLength=4096` on `content`.

The `containerfileArch` enum values are **PascalCase**: `ARM64`, `AMD64`,
`PPC64LE`, `S390X`, `NoArch`, defaulting to `NoArch`. The current emitter writes
lowercase `noarch`, which the v1 API rejects. This is a silent-looking casing
change that breaks validation, so it gets its own acceptance criterion.

The single-entry approach is correct and does not change. This tool generates
one architecture-agnostic Containerfile, and `NoArch` is exactly that meaning.
Per-architecture container files are out of scope.

### `baseImagePullSecret`

v1 adds `spec.baseImagePullSecret` (optional; defaults to the cluster-wide pull
secret). It is **not emitted**. The generated MachineOSConfig is a template the
user customizes, and the cluster-wide default is the right behaviour for one.
Recorded here so it is not rediscovered mid-implementation and mistaken for a
gap.

**Acceptance.**
- Output validates against the v1 schema once placeholders are substituted (the
  hardcoded `REPLACE_WITH_SECRET_NAME` is intentionally not a valid `dns1123Subdomain`).
- Tests assert v1 field names, the `Job` builder type, and the name-matching rule.
- A test asserts `containerfileArch: NoArch` exactly, in PascalCase.
- `renderedImagePushSecret` is emitted at `spec.renderedImagePushSecret`.
- `baseImagePullSecret` is not emitted.
- The 4096-character check is retained.

**Size ceiling cross-reference.** Replacing the per-fragment `COPY` plus `RUN`
pair with a single `RUN --mount` (change 1) should buy some headroom against the
4096-character cap. Worth measuring during implementation, not worth assuming.
See "The MachineOSConfig size ceiling" under Out of scope.

---

## 3. Forced package installs

**What.** A fragment may declare packages it must install to be itself. These
merge into the existing single batched `dnf install`, deduplicated against each
other and against manifest-selected packages.

Manifest `packages:` is unchanged in syntax and meaning.

**One-line answer to the must-vs-can question: declaring packages is always
optional, and a fragment never has to enumerate the contents of a repository it
provides.**

**Why.** An opinionated fragment that ships a repo definition, default
configuration, and service enablement but installs nothing is not a complete
install. Forcing the package it needs makes the fragment a single canonical
artifact a vendor can publish and a consumer can reference. Package lists that
define a unit belong with the unit.

Selection across repositories is different in kind. `packages: [htop, tmux]`
against a bare repo fragment is cherry-picking, and it belongs at the
composition site where that decision is being made. A pure content repository
forces nothing.

Rationale in `docs/rationales.md`, "Packages: what a unit *is* versus what a
build *selects*".

### `packages.available` today

Verified in the current code:

- Parsed from `[fragment.packages] available = [...]`, and read from the
  `io.bootc.fragment.packages.available` OCI annotation.
- **Consumed only by `inspect`, which prints it.** Nothing else reads it.
- `validate_composition` does not use it; it takes the manifest as `_manifest`
  and never inspects package fields at all.
- No validation, conflict detection, dependency analysis, or docs generation
  depends on it.

So the field is inert: it advertises, and nothing acts on the advertisement.

### Specified semantics

- **`required`**: flat list of package names the fragment always installs. New
  field, optional, defaults to empty.
- **`available`**: **removed.** It has no functional consumer, and a field
  nothing reads does not earn its place in the schema. Removing it is a smaller
  change than defining what it means alongside `required`.
- **Manifest selection is free.** It is not constrained to any fragment-declared
  list, and must not become so. dnf resolves names at build time; the muxer
  orders and batches installs without needing to know what a repository
  contains. Mandatory enumeration would be unworkable for a repository the size
  of EPEL and pointless at any size. This matches current behaviour: no change,
  recorded so it is not "tightened" later by accident.
- **Flat list only.** No maps, conditionals, `when:` keys, or per-architecture
  variants. Conditional logic belongs in a hook.

### Collection order and deduplication

The current code builds one list by iterating `manifest.fragments`, flattening
each entry's `packages`, and filtering through a `HashSet`, so the effective
rule today is first-seen wins in manifest order. The merged version keeps that
shape:

- **Order.** All fragment-declared `required` packages first, in the resolved
  fragment order (phase weight, then manifest order), followed by all
  manifest-selected `packages` in manifest order.
- **Dedup key.** Exact string match, the current `HashSet` behaviour. No
  normalization, no version or glob awareness. `postgresql17` and
  `postgresql17-server` are distinct names, as dnf sees them.
- **First-seen wins.** A package declared `required` by a fragment and also
  selected in the manifest appears once, in the required position.
- **Cross-fragment duplicates dedup silently.** Two fragments both requiring the
  same package is normal (a shared dependency), dnf treats a repeated name as
  one install, and a warning here would fire on correct configurations. Not a
  validation error, not a warning.
- Output remains a single batched `dnf install`.

### OCI annotation

The annotation key `io.bootc.fragment.packages.available` is **renamed** to
`io.bootc.fragment.packages.required`. Format is unchanged: a JSON array of
package name strings. The annotation key list in `docs/fragment-format.md` and
the `podman build --annotation` example there both change.

The old key is **not** read, and no alias is kept. `available` and `required`
mean different things, so silently reading one as the other would turn a
catalogue listing into a forced install.

### Migration hazard: unknown keys must not parse silently

`FragmentPackages` derives `Deserialize` without `#[serde(deny_unknown_fields)]`.
If `available` is simply deleted and `required` added, an existing
`fragment.toml` that still declares `available = ["grafana"]` parses
successfully into an empty `required` list. The fragment stops installing
anything and its author gets no error and no warning. The same class of bug
applies to any typo: `requred = ["grafana"]` silently parses into an empty
list.

**Resolution:** add `#[serde(deny_unknown_fields)]` to `FragmentPackages`. This
rejects `available`, any other unknown key, and any typo of `required` with a
parse error naming the offending field. It also rejects unknown keys from newer
fragments read by older tools (forward compatibility); that is the intended
behaviour — forward compatibility (newer fragment read by older tool) is not a
design goal, and an older tool failing on an unrecognised field is the correct
outcome.

The rename-specific error message (`available has been renamed to required`) is
not provided by serde's generic unknown-field error. This is an acceptable
tradeoff: serde's error names the offending field and lists the valid fields,
which is sufficient for the author to find `required`. A bespoke check for one
historical key leaves the general case (typos, future fields) broken.

### Migration

Seven of eight example fragments declare `available`; `cis-hardening` does not.

- **Force what they declare**: `grafana`, `nginx`, `node-exporter`, `tailscale`,
  and `postgresql`. `available` becomes `required`.
  - `postgresql` forces `postgresql17-server` and `postgresql17`. It is an
    opinionated fragment that installs PostgreSQL, not a bare content repo. The
    `postgresql16-server` and `postgresql16` entries in its current `available`
    list are dropped; a consumer wanting 16 selects it in the manifest.
- **Force nothing**: `epel` (`htop`, `tmux`, ...) and `hashicorp` (`vault`,
  `consul`, `nomad`, `terraform`) are catalogues of content repositories. The
  list is dropped; consumers select in the manifest, as they already do.

There is no rule that `repos`-phase fragments are uniformly bare. `postgresql`
is a `repos`-phase fragment that forces packages, and an earlier draft's
generalization to the contrary is withdrawn.

**`postgresql` keeps `phase = "repos"`.** Phase governs the ordering and the
permitted content of a fragment's own `tree/` payload: a `repos` fragment must
carry only repo files and no hooks, which `validate_phase_consistency` enforces.
Forced packages are not fragment payload. They feed the shared batched
`dnf install` that runs after every fragment's repo files have landed, so
declaring `required` says nothing about what the fragment carries and does not
require a phase change. A phase change would only be warranted if `postgresql`
gained configuration files or hooks.

Also update: `fragment.toml` parsing and the `Fragment` struct, the annotation
read path in `loader.rs`, `inspect` output, the annotation key list in
`docs/fragment-format.md`, and the schema section there.

**Acceptance.**
- A fragment declaring `required` produces those packages in the batched install
  with no manifest entry.
- Forced and selected packages deduplicate against each other, exact-string,
  first-seen wins, required before selected.
- Two fragments requiring the same package produce one entry and no warning.
- A manifest may select a package no fragment declares, and this is not an error.
- `postgresql` emits `postgresql17-server` and `postgresql17` with no manifest
  entry, and keeps `phase = "repos"`.
- `available` or any other unknown key in `fragment.toml`'s `[fragment.packages]`
  is a hard parse error (via `#[serde(deny_unknown_fields)]`).
- The OCI annotation is emitted and read as
  `io.bootc.fragment.packages.required`; the old key is not read anywhere.
- Example fragments and docs updated, including the annotation key list and the
  `podman build --annotation` example.

---

## 4. Conditional bootc-specific steps

**What.** Emit `RUN bootc container lint` (weight 90) and `RUN systemctl
preset-all` (weight 35) only when the base is a bootc image, rather than
unconditionally.

**Why.** Everything else the generator emits (`COPY --from`, `dnf install`,
hook execution) works on any RPM base. These two do not: `bootc container lint`
fails on a base without bootc, and `preset-all` is meaningless without systemd.
Making them conditional widens the tool to ordinary container images at low cost.

### How the base is classified

Two signals, checked in this order:

1. **Manifest override.** An optional manifest-level field, `baseType: bootc |
   container`. When present it decides, and no image inspection happens. This
   covers unlabeled custom bases, air-gapped or unauthenticated environments,
   and testing.
2. **The `containers.bootc` image label.** Read from the base image's config
   labels via `skopeo inspect`, which reads metadata only and pulls no layers.
   Upstream bootc base images (CentOS Stream, Fedora, RHEL) carry it. Any
   non-empty value classifies the base as bootc; the value itself is not
   interpreted.

Image name matching is explicitly **not** a signal.

**When the label is absent, the default is `bootc`.** Two reasons:

- It preserves current output for every existing manifest. Both steps are
  emitted unconditionally today, so any other default would change behaviour for
  users who have changed nothing.
- It is the safer of the two failure directions. Misclassifying a plain
  container base as bootc fails loudly and immediately at build time, when
  `bootc container lint` runs on an image without bootc. Misclassifying a bootc
  base as a plain container fails silently: `systemctl preset-all` is dropped,
  the image builds clean, and the fragment-shipped units are simply not enabled.
  That failure surfaces on the deployed node. A loud build-time failure with a
  one-field fix beats a silent runtime one.

**When the label lookup fails** (registry unreachable, authentication missing,
`skopeo` error), classify as `bootc`, warn on stderr naming the base image and
the reason, and continue. Classification is not worth turning into a hard
network dependency, and the manifest override is the deterministic path for
anyone who needs one.

**MachineOSConfig steps are always bootc.** MachineOSConfig exists only to build
OpenShift node OS images, and those are bootc by definition. The MachineOSConfig
artifact always emits both steps, regardless of the base classification result.

Because `--ocp` adds a second artifact rather than switching modes (change 1),
classification always runs — the standalone Containerfile needs it. With
`baseType: container` and `--ocp`, the two artifacts diverge: the standalone
Containerfile omits bootc steps while the MachineOSConfig includes them. This is
not a conflict. The standalone artifact reflects the declared base; the
MachineOSConfig reflects the OCP target, which is bootc by definition. A user
declaring `baseType: container` while also requesting `--ocp` output is building
for two different targets, and the outputs are individually correct.

### Phase table direction

Rather than a `is_bootc` boolean threaded through the generator, tool-managed
steps in the phase table carry a `requires` capability: `systemctl preset-all`
requires `systemd`, `bootc container lint` requires `bootc`. Classification
produces a capability set, and a step is emitted when its requirement is in the
set.

Only one detector exists today, so the mapping is small and deliberately so: a
bootc base yields `{bootc, systemd}`, a plain container base yields the empty
set. Systemd presence on a non-bootc base is not detected and is not assumed;
the empty set is the conservative answer, and `preset-all` is a no-op worth
skipping rather than a step worth guessing at. The `requires` field is the
extension point for the day a second real detector exists (selinux, podman), not
an abstraction to build out now.

**Acceptance.**
- A base classified as non-bootc produces a Containerfile with neither step.
- A base classified as bootc is unchanged from current output.
- The classification signals, their order, and the default are documented in
  `docs/` and the README.
- A base image with no `containers.bootc` label and no manifest override is
  classified bootc. Covered by a test.
- The manifest `baseType` override wins over the label. Covered by a test.
- A failed label lookup warns and classifies bootc, and does not fail assembly.
- The MachineOSConfig artifact emits both steps regardless of classification.
  The standalone artifact reflects the classification. Both are produced by the
  same invocation; classification always runs. Covered by a test.
- No classification path inspects the image name.

---

## 5. Documentation

Rationale for the packages split and the flat-list guardrail is already in
`docs/rationales.md`. The following need updating alongside the code:

- `docs/fragment-format.md`: the `fragment.toml` schema (`required` replaces
  `available`), field constraints, the OCI annotation key list, and the
  `podman build --annotation` example.
- Manifest documentation: the new `baseType` field, its values, and its default.
- Base classification: which signals are checked, in what order, and what
  happens when the label is absent or the lookup fails.
- Hook execution: the bind-mount emission, why COPY was rejected, and a note
  that `RUN --mount=type=bind` requires BuildKit/Buildah (buildah 1.24.0+).

---

## Out of scope

Deferred deliberately; not blocked by anything here.

- **Kickstart and interpreter support of any kind**: fully deferred, including
  carrying config files as fragment payload and any interpreter packaging.
- **The MachineOSConfig size ceiling.** The 4096-character limit caps on-cluster
  builds at roughly six to eight fragments. A bundle-image approach would fix
  it; it needs its own design.
- **Parameterizing the MachineOSConfig placeholders.** The emitter hardcodes
  `REPLACE_WITH_SECRET_NAME` and an internal registry URL for
  `renderedImagePushSpec`. Turning these into CLI parameters is a CLI surface
  change with its own design; the generated object is a template the user edits
  today, and v1 migration does not change that.
- **Per-architecture `containerFile` entries.** `NoArch` is correct while the
  tool generates one architecture-agnostic Containerfile.
- **A second capability detector** for the change 4 phase table (selinux,
  podman, systemd on non-bootc bases). The `requires` field is the extension
  point; no second detector is proposed until a step needs one.
- **Build-time-only repo files.** Repo definitions currently always persist into
  the image, with no way to mark one as needed only during the build.
- **TOML/YAML unification** across `fragment.toml` and the manifest.
- **Deriving `phase` from content** rather than declaring it, given the
  path-based tree-splitting rule already sorts repo files by location.
- **Non-RPM bases.** Supporting apt would mean a second package phase and repo
  convention; a large change, not attempted here.
