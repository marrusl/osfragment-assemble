# Generator and Schema Changes

**Status:** Proposed
**Date:** 2026-07-27

Consolidates five changes to the generator, the OpenShift emitter, and the
fragment schema. Each is independent and can land separately. Change 1 fixes a
defect; the rest are design changes.

---

## 1. Build inputs stay out of the target image

**What.** Execute hooks via a bind mount instead of copying them in:

```dockerfile
RUN --mount=type=bind,from=<fragment>,source=/fragment/hooks,target=/frag-hooks \
    /frag-hooks/configure.sh
```

Applies to hooks and any future non-`tree/` payload. `tree/` content keeps being
copied; that is the delivered payload, not a build input.

Retain the current `COPY` form as an explicit fallback mode (flag-selected).

**Why.** The current emission is a defect. `generator.rs` writes the `COPY` and
the `RUN ... && rm -rf` as separate instructions, so they land in separate
layers. The `rm -rf` only writes a whiteout; the hook bytes remain in the
earlier layer and ship in the final image, recoverable with `podman save`. The
cleanup does not do what it was written to do.

The bind mount is one instruction and one layer, so nothing persists and no
cleanup is needed. It also removes a failure class: hooks can no longer collide
in `/tmp`, and nothing leaks when a hook exits nonzero mid-chain.

The fallback is retained for two reasons. `RUN --mount=type=bind,from=` is a
BuildKit/Buildah extension rather than base Dockerfile, and hand-maintainability
of the generated Containerfile is a stated design goal. More concretely,
on-cluster OpenShift builds have **no build context**; the Containerfile is an
embedded string in a MachineOSConfig. `COPY --from=<registry-ref>` resolves
there because the builder pulls from the registry; whether a bind mount from a
registry image resolves in that environment is unverified. Until it is, a
`COPY`-based path must remain available or the OpenShift output regresses.

**Mode selection.** The user never chooses a mode from knowledge of builder
internals; the target chooses. Default output emits the bind mount, which
podman, Buildah, and current Docker all accept. `--ocp` selects the fallback
form automatically while the MachineOSConfig verification below is open. An
explicit opt-in flag forces the fallback for builders that reject `RUN --mount`;
that failure is immediate and legible (the builder errors on the instruction),
so the flag is an escape hatch discovered at the point of failure, not a
decision required up front. Document the flag next to that error case in the
README so the builder error leads to it.

**Acceptance.**
- Default output executes hooks via `RUN --mount`, with no `COPY` of `hooks/`.
- A test asserts `hooks/` never appears in a `COPY` instruction in default mode.
- Fallback mode reproduces the current form, with the `rm -rf` folded into the
  same instruction as the `COPY` so it is correct in that mode too.
- `--ocp` output uses the fallback form with no flag required, until the
  MachineOSConfig verification below flips it.
- Verify whether `--mount=type=bind,from=<registry-image>` works in a
  MachineOSConfig build pod; record the result. This gates whether the fallback
  is permanent or transitional.

---

## 2. MachineOSConfig v1 migration

**What.** Update `src/ocp.rs` from `machineconfiguration.openshift.io/v1alpha1`
to `v1`:

| Current output | v1 |
|---|---|
| `.../v1alpha1` | `.../v1` |
| `spec.buildInputs.{...}` / `spec.buildOutputs.{...}` | flat: `spec.containerFile`, `spec.imageBuilder`, `spec.renderedImagePushSpec` |
| `imageBuilderType: PodImageBuilder` | `Job` (the only enum value) |
| `spec.buildInputs.renderedImagePushspec` | `spec.renderedImagePushSpec` |
| `spec.buildOutputs.currentImagePullSecret` | removed from spec |

**Why.** The emitted YAML is rejected by current clusters. The `v1alpha1` type no
longer exists upstream, and `PodImageBuilder` is not a valid builder type.

Two current behaviours are correct and must be preserved: the 4096-character
limit on `containerFile.content` is real and enforced, and setting
`metadata.name == spec.machineConfigPool.name` satisfies a CEL validation rule
that would otherwise reject the object.

`containerFile` in v1 is a list indexed by architecture with a maximum of 4
entries. The emitter currently hardcodes a single `noarch` entry; revisit
against the arch-indexed model.

**Acceptance.**
- Output validates against the v1 schema.
- Tests assert v1 field names, the `Job` builder type, and the name-matching rule.
- The 4096-character check is retained.

**Open question for review.** If `v1alpha1` was a deliberate choice targeting an
older cluster, this becomes a documentation change instead: state the supported
version and keep the shape.

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

### Migration

Seven of eight example fragments declare `available`; `cis-hardening` does not.
They split cleanly along the new distinction, which is a useful check on it:

- **Force what they declare**: `grafana`, `nginx`, `node-exporter`,
  `tailscale`. Single-package, opinionated. `available` becomes `required`.
- **Force nothing**: `epel` (`htop`, `tmux`, …) and `hashicorp` (`vault`,
  `consul`, `nomad`, `terraform`) are catalogues of content repositories. The
  list is dropped; consumers select in the manifest, as they already do.
- **Needs a decision**: `postgresql` lists `postgresql17-server`,
  `postgresql17`, and others, and is a `repos`-phase fragment. Either it forces
  the server package and becomes opinionated, or it forces nothing and stays a
  content repo. Recommend forcing nothing, to keep `repos`-phase fragments
  uniformly bare.

Also update: `fragment.toml` parsing and the `Fragment` struct, the annotation
read path in `loader.rs`, `inspect` output, the annotation key list in
`docs/fragment-format.md`, and the schema section there.

**Acceptance.**
- A fragment declaring `required` produces those packages in the batched install
  with no manifest entry.
- Forced and selected packages deduplicate against each other.
- A manifest may select a package no fragment declares, and this is not an error.
- `available` no longer parses; example fragments and docs updated.

---

## 4. Conditional bootc-specific steps

**What.** Emit `RUN bootc container lint` (weight 90) and `RUN systemctl
preset-all` (weight 35) only when the base is a bootc image, rather than
unconditionally.

**Why.** Everything else the generator emits (`COPY --from`, `dnf install`,
hook execution) works on any RPM base. These two do not: `bootc container lint`
fails on a base without bootc, and `preset-all` is meaningless without systemd.
Making them conditional widens the tool to ordinary container images at low cost.

**Design question for review.** The phase table currently hardcodes tool-managed
steps. Making them conditional means the phase system grows a notion of which
tool-managed steps apply to a given base. Worth designing rather than adding a
boolean; this change is proposed, not settled.

**Acceptance.**
- A non-bootc base produces a Containerfile with neither step.
- A bootc base is unchanged from current output.
- How the base is classified is explicit and documented, not inferred from the
  image name.

---

## 5. Documentation

Rationale for the packages split and the flat-list guardrail is already in
`docs/rationales.md`. Schema changes in change 3 require corresponding updates
to `docs/fragment-format.md` (`fragment.toml` schema, field constraints, and the
OCI annotation key list).

---

## Out of scope

Deferred deliberately; not blocked by anything here.

- **Kickstart and interpreter support of any kind**: fully deferred, including
  carrying config files as fragment payload and any interpreter packaging.
- **The MachineOSConfig size ceiling.** The 4096-character limit caps on-cluster
  builds at roughly six to eight fragments. A bundle-image approach would fix it;
  it needs its own design.
- **Build-time-only repo files.** Repo definitions currently always persist into
  the image, with no way to mark one as needed only during the build.
- **TOML/YAML unification** across `fragment.toml` and the manifest.
- **Deriving `phase` from content** rather than declaring it, given the
  path-based tree-splitting rule already sorts repo files by location.
- **Non-RPM bases.** Supporting apt would mean a second package phase and repo
  convention; a large change, not attempted here.
