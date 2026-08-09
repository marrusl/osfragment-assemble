# Secret-Bearing Mount Fragments

Status: Proposed. This is a publication and authoring design layered on
the shipped build-mounts mechanism
(`process-docs/specs/proposed/2026-08-07-build-mounts.md`). It changes
no code. Its subject is the general pattern for any fragment whose
`mount/` carries credential material; RHEL subscription entitlement is
instance one, documented here because it is proven, not because it is
the subject.

Evidence base: a set of empirical runs performed 2026-08-08 on CentOS
Stream 9 (SELinux enforcing, unsubscribed host, digest-pinned fragment
served from that host's own registry) and on a macOS podman machine,
recorded in `process-docs/skills/entitlement-build-mounts.md`, plus a
trace of the current source. Behavioral claims below are of three
kinds, and each is labelled where it appears: measured in one of those
runs, read from source, or inferred from a measured mechanism. Nothing
is taken from vendor documentation.

## The problem this document settles

The build-mounts mechanism answers how credential material reaches the
package step: a `mount/` subtree, presence-derived mount points, a
mandatory digest pin, `ro,z` bind mounts on the batched dnf RUN, never
committed. It deliberately does not answer how such a fragment is
published. A fragment whose layers contain live credentials cannot be
public, yet the pattern it embodies is exactly the thing worth
publishing: which files, at which paths, for which base. Left
unsettled, every consumer rediscovers the file set by trial and error,
and the failed attempts are silent in the worst cases (see the
silent-failure cluster below).

This document settles it with a two-artifact convention and records
the decisions behind it, each with its rationale and its cost, so that
a reviewer has explicit targets rather than prose to excavate.

## Decisions

### 1. Two artifacts, split by name

**Decision.** A secret-bearing mount fragment is published as a pair: a
public example carrying placeholder files at the live fragment's exact
paths, and the real fragment built privately, wherever the real
material is available. The signal that distinguishes the pair lives in
the name (decision 5); the tool does not validate credential material
and does not change for this convention.

**Rationale.** Placeholder material fails at the package step, at the
layer that owns authentication, and the builder commits none of it
either way. The failure is contained, but it is not self-diagnosing,
and this document does not claim otherwise. For instance one the
failure has two forms: missing or wrong CA material produces a
populated generated repo file and a TLS error that names the
certificate problem (curl error 77), which is loud and diagnosable; a
bad entitlement certificate produces an empty generated repo file and
then a package-not-found error that names no credential at all. A
placeholder entitlement certificate is the second kind by
construction, so a composition built from the example fails in the
mode that does not announce itself. (The evidence weights behind the
two rows are given with the diagnostics in the instance-one section.)
The fix is still a one-line `image:` swap in the manifest, but
reaching it requires knowing the pairing convention, which is part of
why decision 5 puts the signal on every surface a reader sees.

The refusal to validate needs stating carefully, because it is three
refusals of different strength, and the original argument ran them
together. Content validation (X.509 well-formedness, expiry, chain
checks) is refused: it would duplicate a check dnf already performs,
and it is custody-adjacent territory decision 6 stays out of.
Scheme-aware validation (knowing that a RHEL entitlement needs these
four files) is refused: it would break decision 2's genericity, one
scheme at a time. Structural validation, checking that a `mount/`
subtree contains the files its own layout implies, with no knowledge
of what any file means, is a third thing that neither objection
touches, and the tool already performs structural mount checks
elsewhere: a file directly under `mount/` is an error, as are
colliding targets and mounts over generator-written paths. It was
considered and is not taken here. The timing argument for it is
recorded so review can weigh it: a structural check fails in seconds
at generation, while every option above fails minutes into a dnf
transaction.

One placeholder construction was considered and rejected: an expired
real certificate. An expired certificate produces a confident wrong
diagnosis. The build reports an expired entitlement, and the reader
goes off to renew a subscription, when the real situation is that
these credentials were never theirs. A wrong signal is worse than no
signal, so the placeholder is obviously fake rather than plausibly
stale.

**Cost.** The failure arrives at build time, not generation time.
Generation treats example and live identically (there is no separate
validation subcommand; the checks run as a phase of generation), so
composing the wrong half is caught by dnf, minutes into a build,
rather than by the tool, seconds into generation, and the error it is
caught with does not name the placeholder. The failure signature is
also scheme-dependent: instance one's two forms are in the record,
other schemes' are not, and a hypothetical scheme that fails open
would fail quietly, with nothing for the tool to notice.

### 2. This is the general pattern, not an entitlement feature

**Decision.** The convention covers any fragment whose `mount/` carries
credential material for package acquisition: RHEL subscription
entitlement, SUSE SCC credentials, mTLS client certificates for
internal mirrors, CA bundles for TLS-intercepting corporate proxies.
The tool knows nothing of any of them.

**Rationale.** The mechanism underneath is already scheme-neutral:
files at declared paths, visible during the package RUN, never
committed. The convention inherits that neutrality for free, and an
entitlement-specific convention would invite an SCC-specific one, then
a mirror-specific one, each a place for distro assumptions to creep
into a tool that has none.

**Cost.** Only instance one is proven. The SUSE, mirror, and proxy
instances are pattern claims: asserted because they reduce to files at
known paths, not because anyone has run them. The acceptance test in
decision 7 is what keeps this honest.

### 3. Repo definitions belong in `tree/`; credentials belong in `mount/`

**Decision.** A fragment that pairs a repository definition with the
credential that unlocks it ships the `.repo` file (and any GPG key)
under `tree/` and the credential under `mount/`. The two halves ride
in one pinnable artifact.

**Rationale.** The two directories have opposite persistence
semantics, and each half needs exactly one of them. A repo definition
is configuration the image should keep: `tree/` copies it verbatim and
the repo-file hoist places it ahead of the package install. A
credential must exist during the install and never after: `mount/`
bind-mounts it for the duration of the dnf RUN and the builder commits
nothing. Putting a credential in `tree/` would write it into a layer
permanently. Putting a repo definition in `mount/` would make it
vanish from the built image, breaking day-2 updates on the deployed
system.

The split has a deliberate escape hatch, and it is not a flag. An
author who wants credential material persisted in the image places it
under `tree/` and takes custody of the consequence by that placement:
`tree/` is already the persistence primitive, so the capability
exists without any new tool surface. There is explicitly no opt-in
flag for persisting `mount/` material; a flag's only function would
be making the anti-pattern ergonomic, and it would duplicate what
placement already expresses.

The day-2 question, what a deployed host uses once the build-time
mount is gone, resolves differently per instance, and is stated here
rather than left for a reader to discover. Instance one resolves
through in-image registration: the `rhel10/rhel-bootc` image ships
`rhc`, `subscription-manager`, and `insights-client` (verified
against that image's manifest, not asserted of RHEL generally;
`centos-bootc:stream10` also carries `rhc`), so the image carries no
credential but carries the acquirer, driven at provision time with an
activation key, a lower-value fleet-scale credential exchanged per
host for host-scoped certificates. The build-time symlink plumbing
plays no part in that story: on a booted host `/run/secrets` is
unpopulated, the container-mode test fails, and subscription-manager
runs in host mode against the real paths. The generic mirror case
resolves through provision-time placement into `/etc`, referenced
from the `tree/` repo file; bootc ships no single opinionated secrets
mechanism, and its own `building/secrets` documentation reserves
machine-local `/etc` and `/var` for exactly this and names the
bootstrap-secret pattern. Mirror mTLS with no provisioning
infrastructure at all is a stated limit of the convention, not a
feature to build.

One category refinement keeps the instances from flattening into a
single kind: a proxy CA bundle is not a secret. It is public trust
material, and it can legitimately ride `tree/` and persist in the
image; it appears among the `mount/` instances because a shop may
prefer not to bake a proxy's CA into every image, not because
confidentiality demands the mount.

**Cost.** Authors must understand the split; a credential accidentally
placed in `tree/` builds successfully and leaks. The tool does not
police the boundary, because deciding which bytes are credentials is
content inspection, and the tool does not inspect content, so the
escape hatch above is the same act performed deliberately and nothing
in the artifact distinguishes intent. The masking semantics also
differ: `COPY tree/ /` merges, a bind mount shadows its
target directory, so moving a file between the halves changes more
than persistence.

### 4. The real fragment lives in a registry the consumer controls

**Decision.** The live half of the pair is pushed only to a registry
its consumer controls. A local registry (`localhost:5000`) is a test
stand-in, not a destination, and no vendor registry is named or
assumed anywhere in the convention.

**Rationale.** Pull access to the live fragment equals possession of
the credential; the build-mounts spec's security posture already
states this. The only party who can correctly scope that access is
the party who owns the credential, so the registry choice is theirs by
construction. Naming a vendor would also quietly convert a neutral
convention into an endorsement.

**Cost.** A standing runtime dependency. `load_registry_fragment`
resolves every fragment's digest on every generation
(`src/loader.rs`), pinned or not, so the consumer's registry must be
reachable on every generation run, not only when the pin was minted
(there is no separate validation subcommand; validation runs as a
phase of generation). For a private registry that is normally true;
for an air-gapped generation host it is a real constraint, and this
document does not soften it.

### 5. The `-example` suffix, on the fragment name and the repository

**Decision.** The example appends `-example` to both the
`fragment.toml` `name` and the image repository:
`rhel-entitlement-example` at `<registry>/rhel-entitlement-example`,
beside a live `rhel-entitlement`. The suffix is reserved by
convention: a fragment name ending in `-example` asserts that its
`mount/` material is placeholder. The signal lives in the name; there
is no tool change and no validation behind it.

**Rationale.** The suffix must ride the repository as well as the
name, because of what each surface actually carries. Mount-carrying
manifest entries are digest-pinned by rule, and a pinned reference
shows no tag, so a tag-only convention would be invisible in the
manifest and in the emitted mount reference. And of the fragment's
identifying fields, the repository is the only one that reaches the
generated Containerfile's load-bearing text: the emitted
`--mount=...,from=` carries the image reference, never the
`fragment.toml` name, and self-contained output emits no `from=` at
all, carrying the fragment name only in its context `source=` path.
The `fragment.toml` name and version reach `inspect`, which parses
the in-layer TOML; the name appears in the generated Containerfile
only as the `# Fragments:` header comment, which OCP output omits.
The `inspect` mount section itself cannot carry the distinction: it
prints derived targets only, and a correct example derives targets
identical to the live fragment's by construction, deliberately
(decision 1: the tool does not validate material). With the suffix on
both name and repository, the pair is distinguishable in both
directions on every surface that shows either field, and the name
annotation mirrors `fragment.toml`, so annotations inherit the suffix
with no new keys.

**Cost.** Convention-only enforcement, in two layers. Nothing stops
an author publishing live material under an `-example` name or
placeholder material under a bare one; the suffix is an assertion,
not a proof. And the surfaces are not equally authoritative. `inspect`
and generation read the name from the in-layer `fragment.toml`, which
cannot drift because it is the layer. `list` does not: whenever the
metadata and mounts annotations are present, it takes the annotation
fast path, never pulls a layer, and prints the name from
`com.github.marrusl.osfragment.name`. On `list` the name is a
hand-authored annotation, so the anti-drift argument for carrying the
signal in the name holds on two of the three surfaces this decision
names and not on the third. A dedicated "this is placeholder"
annotation was still rejected: it would be hand-authored metadata on
every surface, with no layer counterpart anywhere, while the name has
a layer-authoritative source on the surfaces that feed builds.

### 6. What the tool stays out of

The mechanism authenticates package acquisition and takes no position
on custody. Rotation cadence, credential version naming, who authors
the real fragment, and vendor-issued subscription containers are all
outside this design, in the same proportion as the existing refusal to
own signing.

### 7. The acceptance test is genericity

**Decision.** The design is accepted if any authentication scheme that
reduces to files at known paths during the package step is expressible
with zero tool changes, and rejected if the RHEL instance needed even
one entitlement-aware line of code. It needed none.

**Rationale.** The tool's whole posture is composing inputs while
owning none of the bordering domains. A convention that only works for
entitlement would be evidence the mechanism is not actually general.

**Cost.** The test has a converse, and this document states it rather
than leaving it for review to find: schemes that do not reduce to
files are out. The expressiveness boundaries are given their own
section below, and one placement decision the test bites on directly
is presented after them, as a decision left open rather than a
boundary.

## The convention, stated generically

A secret-bearing mount fragment is an ordinary fragment. Its `mount/`
subtree carries the credential files at the exact paths the
acquisition code reads during the build, and everything the
build-mounts mechanism already enforces applies unchanged: presence
derivation, the mandatory digest pin, collision checks, `ro,z`, never
committed.

The example is the same fragment with the bytes replaced. It must
ship real files: derivation is presence-based, collecting only regular
files (`extract_layer_entries`, `src/loader.rs`) and deriving one
mount point per directory that directly contains a file
(`derive_mount_points`, `src/mount.rs`). A fragment cannot declare a
mount target without shipping a file at it, so a "declaration-only"
example is structurally impossible: a fileless `mount/` derives
nothing, escapes the mandatory pin (`check_mount_digest_pins` skips
fragments with no derived points), and emits no `--mount` flag. These
are source-trace claims; the fileless path was read, not executed.
Either way the conclusion stands: that is not a degraded example, it
is a fragment with no mounts at all.

Placeholder files therefore sit at exactly the live paths. Identical
paths make the pair derive identical targets, which buys behavioral
parity in the tool's checks: the example is subject to the same
digest-pin requirement and the same `--materialize-mounts` gate as
the live fragment, so a consumer can rehearse the composition by
running the same generation the live half would get. There is no
separate read-only check surface: the tool has no `validate`
subcommand, and the pin, collision, and notice checks run as a phase
of generation. Parity has one exception before the package step, and
it runs in the safe direction: under `--self-contained
--materialize-mounts` the example writes placeholder bytes into the
build context and its sibling archive, where the live fragment would
write real credential material durably to disk, which is exactly the
custody change the tool's own gate warns about. Past that, the
difference is the package-step failure placeholder material produces,
stated honestly under decision 1: for instance one, an empty
generated repo file and a package-not-found error, not a failure that
names the credential.

## Instance one: RHEL subscription entitlement

Claims in this section are measured unless labelled otherwise; the
labels mark the few that are inferences from a measured mechanism. The
experiments are recorded in
`process-docs/skills/entitlement-build-mounts.md`.

### The mount target is `/run/secrets/`, forced by control flow

`rhsm/config.py` in the base image decides it is running in a
container by testing two paths the base ships as symlinks into
`/run/secrets/`: `/etc/rhsm-host/` must be a directory, and
`/etc/pki/entitlement-host/` must be a directory **and non-empty**
(`any(os.walk(...))`). The non-emptiness clause is load-bearing twice
over: it is why an unsubscribed host's empty injection does not flip
container mode, and why the decoy in the experiment below had to be
non-empty. When the test passes, configuration is read from
`/etc/rhsm-host/rhsm.conf` rather than `/etc/rhsm/rhsm.conf`, which is
the mechanism behind the `rhsm.conf` membership claim in the minimum
set, and the parser rewrites `ca_cert_dir`, `repo_ca_cert`, and
`entitlementCertDir` (the last only when it holds the default value)
to the `-host` paths. The real paths stop being consulted, and the
`repo_ca_cert` rewrite is the one the minimum-set section turns on.

The decisive experiment: a build mounting a good entitlement at the
real path `/etc/pki/entitlement`, plus a non-empty decoy at
`/run/secrets/etc-pki-entitlement`, fails with the fragment's material
silently ignored: the decoy flips container mode, `entitlementCertDir`
is redirected to the `-host` path, and the real-path mount is never
consulted. That decoy condition is the expected standing state on a
subscribed build host, an inference from the `mounts.conf` symlink
chain below; no subscribed host was itself tested. The real-path
model works in isolation and loses on exactly the hosts most likely to
build RHEL images. `/run/secrets/` is the only target the host cannot
override, so it is the target.

### The real-path model is baked into the shipped surfaces

The pattern that experiment defeats is the worked example across the
shipped documentation and the tool's own error text.
`docs/fragment-format.md` teaches it four times: the `mount/` anatomy
block, the derivation example, the emitted-form example, and the
mounts-annotation example. `docs/design.md` and
`docs/design-overview.md` teach it in their `mount/` paragraphs. Four
user-facing strings in source teach it as well: the mount-target
rejection at `src/mount.rs:80`, the file-directly-under-`mount/`
error at `src/mount.rs:169`, the empty-mount notice at
`src/mount.rs:214`, and the generator-written-path collision error at
`src/validate.rs:232`. Three of those four fire at exactly the moment
an author is fixing a mount mistake, which is the worst moment to
hand them the pattern that fails silently on subscribed hosts. One
instance is sharper still: the format doc's derivation example mounts
`/etc/rhsm`, the precise target the boundaries section below names
for the `syspurpose/` masking hazard. Two qualifiers keep this
honest: the real-path model is not wrong everywhere, only on hosts
whose injection populates `/run/secrets`, which is the population
instance one argues matters most; and none of these surfaces is
edited in this pass. The scope is recorded here so the follow-up that
lands the proposed format text below can sweep all of them in one
change instead of leaving the docs and the error text teaching what
the format text warns against.

### The minimum viable set

Four files, and the durable statement of the set is by role rather
than by filename: the entitlement certificate, its key, `rhsm.conf`,
and whatever file `repo_ca_cert` in that `rhsm.conf` resolves to. For
a stock CDN-direct configuration that resolution is `redhat-uep.pem`.
Because `repo_ca_cert` is read from the mounted `rhsm.conf`, any
configuration that points it elsewhere changes which file is required;
Satellite, Katello, and proxied setups are the expected such cases,
none of them tested. The by-role statement stays correct where a flat
filename list would not.

```
mount/run/secrets/etc-pki-entitlement/<serial>.pem       # entitlement cert
mount/run/secrets/etc-pki-entitlement/<serial>-key.pem   # its key
mount/run/secrets/rhsm/rhsm.conf
mount/run/secrets/rhsm/ca/redhat-uep.pem                 # what repo_ca_cert resolves to
```

Each membership claim has an isolation result behind it:

- Cert and key: neither alone passes; the non-empty entitlement
  directory is also what flips the container-mode test.
- `rhsm.conf`: without it, `repo_ca_cert` interpolation fails and the
  error surfaces as a raw `%(ca_cert_dir)s` string inside a curl
  message, which does not look like a missing-config problem. This is
  the member a reviewer will try to cut, and the isolation run is the
  answer.
- `redhat-uep.pem`: the base ships it, but at a path the container-mode
  rewrite makes unreachable, so the fragment must carry it. A variant
  shipping only this CA passes; variants shipping only the other CA,
  or none, fail with curl error 77.
- `redhat-entitlement-authority.pem` is present in the base and in host
  dumps and is not shipped. Three findings close it out: a recursive
  grep across the base's `site-packages` trees and `/etc/rhsm` returns
  zero references to it; the file that is consulted is named by the
  hardcoded default `"repo_ca_cert": "%(ca_cert_dir)sredhat-uep.pem"`;
  and the one code path that could have used it, the directory trust
  store for the Candlepin connection, is short-circuited by
  not-in-container guards on its callers (the dnf plugin and the
  release-version lookup), regardless of `full_refresh_on_yum`, so no
  configuration setting brings the file into play during a build.

`redhat.repo` is not needed and not shipped. `rhel-bootc:latest`
(10.2, measured) ships a completely empty `/etc/yum.repos.d/`; the
subscription-manager dnf plugin generates the repo configuration at
build time from the mounted certs, and a verification build confirms
the generated entries name them in their `-host` form
(`sslclientcert = /etc/pki/entitlement-host/<serial>.pem`, the serial
a placeholder here as everywhere in this document). Shipping
`redhat.repo` would also put a file directly in `mount/run/secrets/`,
which makes `run/secrets` itself a mount point and collapses the two
intended mounts into one by the nesting-prune rule. That collapsed
variant passes, and fully masks the host, but it must be a deliberate
choice, never an accident of including one extra file.

The serial in the layout above is a placeholder and must stay one: a
real entitlement serial identifies a subscription, so neither the
example nor any public document carries a real one. The example's
placeholder files use an obviously fake serial such as
`0000000000000000.pem`.

### The confound any reproducer will hit

`containers-common` ships a `mounts.conf` that injects
`/run/secrets` from `/usr/share/rhel/secrets` into every container and
every build step. Measured on two builders, Fedora CoreOS 44 and
CentOS Stream 9, with the same package shipping the same content in
both; extending that to the rest of the RPM family is an extrapolation
from those two points, not a survey. On a macOS podman machine the
injection arrives pre-loaded with both CA certificates (measured); the
attribution of those files to the VM's
`subscription-manager-rhsm-certificates` package is inferred, not
queried on the VM. A naive subtraction test on such a host concludes the CA
files are unnecessary; they are not, the host was supplying them. The
runs behind this document were controlled for it with two negative
controls (nothing mounted must fail; an empty directory mounted over
`/run/secrets` must also fail) and by re-verifying that the passing
results shadowed every path the host could have populated.

### Two diagnostics worth keeping

Two facts that separate failure modes which otherwise present
identically, with unequal evidence behind them:

- Repo generation does not depend on the CA: all four CA-isolation
  variants produced a fully populated `redhat.repo`, including the two
  that failed. So a populated `redhat.repo` together with curl error
  77 means the CA file is missing. The converse diagnostic, an empty
  `redhat.repo` pointing at the entitlement certificate, rests on a
  single observation (the clock-skew incident recorded with the runs)
  and carries correspondingly less weight.
- `sslcacert` in the generated output is copied from `repo_ca_cert`
  whether or not the file exists, so its presence in `redhat.repo` is
  not evidence the CA was mounted.

## Why the mandatory pin holds here, twice over

The build-mounts spec pinned mount-carrying fragments for a trust
reason: a movable tag on an artifact that injects trust material is an
invisible substitution point. The empirical work adds a second,
independent argument. The two cover disjoint failure classes rather
than ranking against each other: the trust argument covers adversarial
tag movement, the mechanical argument covers accidental staleness, and
which one is load-bearing depends on the builder. On a long-lived
builder with populated local storage, staleness is the live hazard; on
an ephemeral CI builder, local storage is empty, nothing stale exists
to shadow anything, and the mechanical argument is vacuous exactly
there while the trust argument is untouched. Neither dominates, both
are needed, and the pin is the single control that serves both.

At build time, `RUN --mount=from=<ref>` resolves against local storage
first and contacts the registry on a miss. That sentence packs three
claims, and they carry different evidence. Pull on miss is measured on
the CentOS Stream 9 host: the digest-pinned proof build printed the
pull and proceeded, after local storage was verified empty. That local
storage can satisfy a mount without the registry is also on the
record, but as a byproduct of the pre-clock-fix session rather than a
designed control: clock skew had blocked the `registry:2` pull, so the
registry never started (`curl` against it returned `000`, the push
failed connection refused), the fragment was built locally from
`scratch`, and a tag-referenced mount then resolved and delivered its
file with no pull line appearing. What that sequence shows is
satisfaction without a registry; it cannot show preference, because no
registry copy existed to lose to. The third claim, that local content
is preferred over a registry copy under the same tag, is the shadowing
hazard the pin argument actually rests on, and its evidence is the
repository's own recorded 2026-08-04 incident
(`process-docs/skills/registry-verification.md`): a stale local
`grafana:11.0` shadowed the good published fragment and produced an
exit-127 failure. A digest reference closes that hazard, and this step
is reasoning from the mechanism rather than a measurement: local
storage is content-addressed, so a local hit on `@sha256:X` resolves
to the same bytes the registry would have served. For the one
fragment class where stale trust material is the worst possible
substitution, the pin rule closes the hazard as a side effect of its
trust purpose.

The enforcement itself is registry-agnostic and network-free, and the
claims about it come from reading the source, not from the runs: the
check is a text test on the manifest's own image reference
(`src/validate.rs:159`, `declared.contains("@sha256:")`), and
`--pin-digests` does not satisfy it, because the digest must live in
the user's own `image:` line to survive. The rejection path, a
tag-referenced fragment with derived mounts, was not exercised in the
runs; what the runs vouch for is the accepting side, where
digest-pinned port-carrying references (`localhost:5000`,
`localhost:5050`) generated and built correctly, so port-carrying refs
do pin. The rule behaves identically for a test stand-in and a real
private registry.

## The silent-failure cluster

Six ways to be wrong with no signal, all in the path that carries
credentials. They are named here precisely so review can weigh them;
this document proposes no mechanism changes for them beyond what the
convention itself requires.

1. **A partial file set silently moves the derived mount root.**
   Derivation collects every directory that directly contains a file,
   then prunes nesting. Drop `rhsm/rhsm.conf` from the four-file set
   while keeping `rhsm/ca/redhat-uep.pem`, and the derived targets
   change from `/run/secrets/etc-pki-entitlement` plus
   `/run/secrets/rhsm` to `/run/secrets/etc-pki-entitlement` plus
   `/run/secrets/rhsm/ca`: the mount root moves, `/run/secrets/rhsm`
   is never mounted at all, and `rhsm.conf` is absent from the build.
   Nothing signals it. The derived set is non-empty, so the
   empty-mount notice stays quiet; the digest pin is satisfied; the
   collision checks pass; `inspect` prints two targets that look
   plausible. The failure surfaces as the raw `%(ca_cert_dir)s` curl
   error the minimum-set section documents, pointing at a CA problem
   rather than a missing mount. Partial file sets are exactly what
   placeholder example fragments invite, which is why this item leads
   the list.

2. **An empty subdirectory beside a populated one derives nothing,
   silently.** The empty-mount notice fires only when the entire
   `mount/` is fileless (`empty_mount_notice`, `src/mount.rs`). A
   partial example that ships some paths as bare directories quietly
   mounts less than the live fragment does, passes generation's
   checks, and fails at dnf with an error that points at credentials
   rather than at the missing mount. The convention's requirement that
   placeholder files exist at every live path is the exposure surface
   for this, which is why the proposed format text below states the
   requirement as a rule rather than a suggestion.

3. **A decoy at `/run/secrets/etc-pki-entitlement` silently defeats a
   real-path mount.** The decoy build is measured. That the decoy
   condition is the standing state on any subscribed build host is
   inferred from the `mounts.conf` symlink chain; no subscribed build
   host was itself tested. The chosen
   `/run/secrets/` target avoids it for instance one; any future
   instance whose consumer implements a similar host-redirect must be
   traced the same way before its paths are documented, because the
   failure produces no signal that a mount was ignored.

4. **A stale `target/release` binary emits a Containerfile with no
   mount flags and exits zero.** The mount-free Containerfile and the
   zero exit were observed directly, from a binary two days older than
   `src/mount.rs` that also printed no `mount/` section from
   `inspect`. The downstream consequence, dnf failing much later with
   a package-not-found error, was not run to completion; it is
   inferred from the negative controls, which fail exactly that way
   whenever no entitlement is mounted. Whether
   the release binary belongs in the tree at all is a repo hygiene
   question this document leaves open; it is listed because its
   failure lands in the same credential path with the same absence of
   signal.

5. **A `mounts` annotation of `[]` makes `list` hide the mounts
   entirely.** When a fragment's metadata and mounts annotations
   parse, `list` takes the annotation fast path and never pulls a
   layer. An annotation holding the literal empty list is a successful
   parse, so a fragment carrying live credential material appears in
   `list` as an ordinary fragment with no mounts: no mounts line, and
   no closing note that material is mounted during the package step.
   No drift warning is possible on that path, because the drift check
   needs the layer the fast path deliberately does not pull. The layer
   stays authoritative for generation, so builds are unaffected; what
   fails silently is the survey surface a consumer would use to ask
   which fragments mount material.

6. **A RHEL entitlement paired with a CentOS base produces the same
   symptom with nothing wrong in the mechanism.** Measured: on
   `centos-bootc:stream10` the same fragment enters container mode,
   `redhat.repo` is written, zero RHEL repositories come up enabled,
   and the install fails package-not-found. The gap is the product
   certificate, which maps entitlement content sets onto enabled
   repositories; the CentOS base ships none, and the mount plumbing
   works identically on both bases. Unlike the other five, this one
   is not the mechanism's fault, and a user picks their base
   knowingly, so it gets no warning prose in user-facing docs; a
   correct example manifest is the instrument for it. It earns its
   line here because at debugging time the symptom is
   indistinguishable from the other five: a correct-looking build
   failing package-not-found, with no message naming the actual gap.

## Limits, stated as boundaries

Three boundaries of the design, not defects discovered later. The
first two are limits of what the mechanism can express, stated so the
acceptance test in decision 7 has explicit edges; the third is
builder-side custody: what persists on a builder that has pulled the
live half, and what label those bytes wear.

1. **Environment-variable authentication cannot be expressed.**
   Derivation is presence-based on files; no file means no mount
   point. A scheme whose credential exists only as a token in an
   environment variable or a command-line flag has no fragment
   expression. Per-build secret plumbing (`podman build --secret`)
   remains the right tool there, as the build-mounts spec already
   notes for the single-pipeline case.

2. **Derivation mounts directories, not files.** A bind mount shadows
   its entire target directory for the duration of the RUN. A
   credential that belongs as one file inside a directory holding
   other needed content masks the rest. Instance one dodges this
   because the `/run/secrets/` subdirectories hold nothing but the
   credential material; mounting `/etc/rhsm` instead would mask
   `syspurpose/`, which the base populates (measured on UBI 10;
   `facts/` exists there too but is empty, so the masking cost rides
   on `syspurpose/` alone). An
   instance whose credential path sits inside a load-bearing directory
   has no clean expression today.

3. **Builder-side custody: the resident bytes, and their label.**
   Two facts about any builder that has pulled the live half, stated
   as facts about the mechanism and no further. First, deletion does
   not do what it looks like. Measured twice in the evidence base,
   both times the hard way: after `podman rmi` by tag, the image
   survives as `repo:<none>`, still holding the credential material;
   it retains a repository name, so it is not "dangling" and
   `podman image prune -f` does not remove it either. Removal
   requires the image ID, and verification requires a filename search
   of the storage tree rather than reading `podman images`. Second,
   the resident bytes wear a shared label. Build mounts are emitted
   `ro,z`, and `z` applies a shared container SELinux label to the
   fragment's content in local storage, readable by any container of
   the shared container type on that builder. `z` rather than `Z` is
   deliberate: `Z` would relabel files on a self-contained build,
   where the mount source is a materialized directory in the user's
   own build context, stamping a private per-container category onto
   files the user owns, permanently, a known podman footgun rather
   than a theoretical concern. Nor can the option be split by mode:
   `MountPoint::mount_flag` (`src/mount.rs:116`) owns the flag's byte
   format in exactly one place for both emission forms, deliberately,
   so they cannot drift; one flag serves two source kinds, and `Z` is
   wrong for one of them. Decision 4 governs who can pull the live
   fragment; nothing in this design governs what persists on a
   builder afterwards or how it is labelled, and the never-committed
   guarantee does not cover it, because the residue lives in the
   builder's image store, not in a committed layer. What anyone
   should do about any of this is custody, out of scope by decision 6.

## Where mounts attach: a decision left open

Mount material lands on the batched package RUN only, and that is a
placement decision, not a mechanism boundary. The code settles the
factual half: the generator attaches the derived mount flags
exclusively to the dnf RUN (`src/generator.rs`, the build-mounts block
feeding the package RUN emission), and the hook RUN carries exactly
one bind mount, the fragment's own `hooks/` directory at
`/frag-hooks` (`src/generator.rs`, hook emission), with no `mount/`
material. Nothing technical prevents adding a second `--mount` flag to
a hook RUN that already carries one, sourced from the same fragment
image, with the same never-committed property. The exclusion is
chosen, so it is stated here the way the other decisions are, with a
rationale and a cost, and unlike them it is left open for review.

**Decision on record.** Version 1 of the build-mounts mechanism scopes
mounts to the package RUN; hook RUNs receive none, including for a
hook's own package installs. The build-mounts spec records
manifest-level wiring, where the consumer names which fragment's hook
receives which mount, as the extension path.

**Rationale.** The grant to the package step is narrow in a way a
blanket hook grant would not be. It is not that dnf is trusted code
and hooks are not: the package transaction itself runs third-party
rpm scriptlets as root with the mounts readable, and the build-mounts
security posture says so plainly. The difference is the scope and
countability of the grant. Package-step exposure is bounded to one
RUN and to packages the consumer explicitly selected. Extending
mounts to hook RUNs without wiring would expose every composed
credential to every hook-carrying fragment in the composition:
fragment A's entitlement key readable by fragment B's vendor
installer, silently, as a property of composition order rather than
of anyone's decision. The recorded extension path exists precisely to
keep that a per-grant consumer choice.

One inference is worth heading off, because anyone reading the format
will make it: hooks already receive a temporary bind mount, so
mounting credentials there can look like no new exposure. That
conflates persistence with access. Both mounts are equally temporary
and equally uncommitted; the difference is what the mounted bytes
are. The hook mount carries the hook's own files to the hook's own
code. A credential mount on that RUN would carry another party's
secret material into arbitrary third-party code. The persistence
guarantee is unchanged either way; the access grant is not.

**Cost.** Private language-package indexes (pip, npm, cargo)
authenticate from files and are installed in hooks, because they are
not RPM content and the dnf phase never sees them. That is plausibly
the most common file-based authentication case outside RPM
repositories, and the current placement cannot reach it. Decision 7
makes genericity across file-based auth the acceptance test for this
design, and this is the sharpest place that test bites: the mechanism
is generic across acquisition schemes that authenticate the package
step, and stops at the step boundary, not at the file boundary.

This document does not resolve the tension. The options on the table
are the recorded manifest-level wiring, or leaving hook-phase
credentials permanently out of scope; choosing between them needs
review input on whether the language-index case is demand or
hypothesis, and it is deliberately not chosen here.

## Proposed addition to `docs/fragment-format.md`

To append to the `## mount/ Directory Layout` section, verbatim, once
this design is approved. It is quoted here rather than applied.

> ### Example fragments for mounts that carry credentials
>
> A fragment whose `mount/` carries live credential material
> (entitlement certificates, mirror client certificates, proxy CA
> bundles) is normally published as a pair: a public **example**
> carrying placeholder files, and a live counterpart carrying the real
> material, built wherever that material is available. The convention
> below keeps the two from being mistaken for each other.
>
> An example must ship real files, at exactly the paths the live
> fragment uses. Derivation is presence-based: a mount point exists
> only when a directory under `mount/` directly contains a regular
> file, so a fragment cannot declare a mount target without shipping a
> file at it, and an empty subdirectory beside a populated one derives
> nothing for the empty path, without a notice, and dropping a single
> file from a set can silently move a derived mount root. Identical
> paths make the pair derive identical targets, so `inspect` shows the
> same `mount/` section for both, and switching between them is a
> one-line `image:` change in the manifest. Placeholder material does
> not authenticate, and the failure does not name the placeholder: for
> entitlement-style credentials a placeholder certificate produces an
> empty generated repo file and a package-not-found error, while only
> missing CA material fails with a TLS error that names a certificate.
> Diagnosing a failed build therefore starts from which half of the
> pair the manifest names, not from the error text.
>
> Naming carries the distinction, on both the fragment name and the
> image repository. The example appends `-example` to the live
> fragment's name (`rhel-entitlement-example` beside
> `rhel-entitlement`) and publishes under a repository name with the
> same suffix. Both halves are needed because the surfaces differ. A
> manifest entry pinned by digest shows no tag at all, so a tag-only
> convention would be invisible on every surface a review reads; the
> repository rides the manifest `image:` reference and the generated
> `--mount=...,from=`, which carries the image reference (and
> self-contained output, which emits no `from=`, names the fragment
> in its `source=` path instead). The fragment name is what `inspect`
> prints from the in-layer `fragment.toml` and what the generated
> Containerfile records in its header comment; `list` prints the name
> from the fragment's annotations when they are present, without
> pulling a layer. No annotation work is needed: the name annotation
> mirrors `fragment.toml` and carries the suffix with it, and the
> mounts annotation is identical between the pair by construction. A
> fragment name ending in `-example` is reserved by this convention
> to assert that its `mount/` material is placeholder.
>
> Both halves of the pair derive mount points, so both are subject to
> the digest-pinning requirement above. The live half belongs in a
> registry its consumer controls, since pull access to it equals
> possession of the credential.

## Observed in passing, not addressed here

Three pre-existing items adjacent to this design, recorded so they are
not lost; none is fixed in this pass and none affects the decisions
above.

- The comment at `src/inspect.rs:31-34` says the annotation fast path
  is used inside `load_registry_fragment` with the layer still pulled
  for tree paths. `src/loader.rs:734-739` says, and does, the
  opposite: assembly always parses the in-layer `fragment.toml`, and
  the fast path is limited to metadata-only operations. The loader is
  right, and the stale comment misdescribes exactly the provenance
  distinction decision 5's cost paragraph turns on.
- The collision error at `src/validate.rs:248-254` explains the
  refusal partly with a nested-mount shadowing claim describing a
  situation that derivation pruning makes unreachable in emitted
  output. The error's other stated reason, first-wins credential
  mysteries, carries the refusal on its own.
- `docs/design.md` says a fragment is an OCI image carrying three
  things, then lists four; the count predates `mount/` and was never
  updated in the full explainer.

## What was not verified

Stated plainly, so review can weigh confidence rather than guess it.

- **Only the RHEL instance is proven end to end.** The SUSE SCC,
  mirror mTLS, and proxy CA instances are asserted from the pattern
  (files at known paths during the package step) and have not been
  run. The SUSE case in particular involves a different acquisition
  consumer and deserves the same control-flow trace instance one got
  before its paths are documented.
- **No Debian-family builder was tested.** The `mounts.conf`
  auto-injection is RPM-family packaging, so an Ubuntu builder is
  expected to be the strictly simpler case with no host contribution
  at all, but that is an expectation, not a measurement.
- **No subscribed build host was tested.** The decoy experiment
  simulates the subscribed-host condition; that the condition actually
  obtains on such hosts is inferred from the `mounts.conf` symlink
  chain, which was measured only on unsubscribed machines.
- **The isolation evidence spans two environments.** The cert, key,
  and `rhsm.conf` isolations ran on UBI 10 on the macOS podman
  machine; only the CA isolation ran on `rhel-bootc` on the CentOS
  Stream 9 host. The bases were measured to match on every consulted
  path, but the full subtraction ladder was not repeated on
  `rhel-bootc`.
- **No example composition was built end to end.** The failure
  signature stated for placeholder material in decision 1 is derived
  from the recorded certificate-defect runs (entitlement absent, cert
  without key, key without cert) and the clock-skew incident, not
  from a build of an actual example fragment carrying placeholder
  bytes. The two-row statement is the honest reading of that record;
  a placeholder-bytes build has not itself been run.
- **OpenShift on-cluster layering was not exercised for this
  convention.** The build-mounts spec records that entitlement
  fragments are unnecessary there; the remaining instances on that
  path ride the one-authfile model and are untested.
- **The proposed format text is not applied.** `docs/fragment-format.md`
  is unchanged by this document; the quoted block above is the
  proposal.
