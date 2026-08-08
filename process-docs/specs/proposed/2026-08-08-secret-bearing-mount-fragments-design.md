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
trace of the current source. Every behavioral claim below cites one or
the other. Nothing in this document is inferred from vendor
documentation.

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
public exemplar carrying placeholder files at the live fragment's exact
paths, and the real fragment built privately, wherever the real
material is available. The tool does not validate credential material
and will not grow validation for this.

**Rationale.** Placeholder material fails loudly and at the right
layer: a composition built from the exemplar reaches the package step
and dies with an SSL error against exactly the repository that needed
the real material. The failure is attributable and the fix is a
one-line `image:` swap in the manifest. Credential validation in the
tool would duplicate a check dnf already performs, would need
per-scheme knowledge the tool refuses to carry (X.509 for one scheme,
key-value text for another), and would turn a neutral byte mover into
a policy engine.

**Cost.** The failure arrives at build time, not generation time.
`validate` and `generate` treat exemplar and live identically, so
composing the wrong half is caught by dnf, minutes into a build,
rather than by the tool, seconds into generation. A reviewer should
also note that the loud-failure claim is scheme-dependent: it holds
where acquisition authenticates over TLS (every instance named in
decision 2); a hypothetical scheme that fails open would fail quietly,
and the tool would not notice.

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

**Cost.** Authors must understand the split; a credential misplaced
into `tree/` builds successfully and leaks. The tool does not police
the boundary, because deciding which bytes are credentials is content
inspection, and the tool does not inspect content. The masking
semantics also differ: `COPY tree/ /` merges, a bind mount shadows its
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
reachable at each `generate` and `validate` run, not only when the pin
was minted. For a private registry that is normally true; for an
air-gapped generation host it is a real constraint, and this document
does not soften it.

### 5. The `-exemplar` suffix, on the fragment name and the repository

**Decision.** The exemplar appends `-exemplar` to both the
`fragment.toml` `name` and the image repository:
`rhel-entitlement-exemplar` at `<registry>/rhel-entitlement-exemplar`,
beside a live `rhel-entitlement`. The suffix is reserved by
convention: a fragment name ending in `-exemplar` asserts that its
`mount/` material is placeholder.

**Rationale.** Two structural facts force the distinction into the
name and repository, because no other surface can carry it. First, the
`inspect` mount section prints derived targets only, and a correct
exemplar derives targets identical to the live fragment's by
construction, so that surface cannot tell them apart, deliberately
(decision 1: the tool does not validate material). Second,
mount-carrying manifest entries are digest-pinned by rule, and a
pinned reference shows no tag, so a tag-only convention would be
invisible in the manifest, in the emitted `--mount=...,from=`, and in
the Containerfile header. With the suffix on name and repository,
every reading surface distinguishes the pair in both directions: an
exemplar composed by mistake announces itself in `inspect`, `list`,
and the generated Containerfile, and a live fragment can never be
mistaken for the public artifact. The name annotation mirrors
`fragment.toml`, so annotations inherit the suffix with no new keys;
the mounts annotation is correctly identical between the pair.

**Cost.** Convention-only enforcement. Nothing stops an author
publishing live material under an `-exemplar` name or placeholder
material under a bare one; the suffix is an assertion, not a proof.
The alternatives were weaker, not stronger: a "this is placeholder"
annotation is metadata that can drift from the layer, while the name
cannot drift because it is the layer.

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
files are out. Two hard boundaries are given their own section below,
and one placement decision the test bites on directly is presented
after them, as a decision left open rather than a boundary.

## The convention, stated generically

A secret-bearing mount fragment is an ordinary fragment. Its `mount/`
subtree carries the credential files at the exact paths the
acquisition code reads during the build, and everything the
build-mounts mechanism already enforces applies unchanged: presence
derivation, the mandatory digest pin, collision checks, `ro,z`, never
committed.

The exemplar is the same fragment with the bytes replaced. It must
ship real files: derivation is presence-based, collecting only regular
files (`extract_layer_entries`, `src/loader.rs`) and deriving one
mount point per directory that directly contains a file
(`derive_mount_points`, `src/mount.rs`). A fragment cannot declare a
mount target without shipping a file at it, so a "declaration-only"
exemplar is structurally impossible: a fileless `mount/` derives
nothing, escapes the mandatory pin (`check_mount_digest_pins` skips
fragments with no derived points), and emits no `--mount` flag. That
is not a degraded exemplar; it is a fragment with no mounts at all.

Placeholder files therefore sit at exactly the live paths. Identical
paths make the pair derive identical targets, which buys behavioral
parity in every check: the exemplar is subject to the same digest-pin
requirement and the same `--materialize-mounts` gate as the live
fragment, so a consumer can rehearse the entire composition, and the
only difference at any point is the dnf-layer authentication failure
that placeholder material produces.

## Instance one: RHEL subscription entitlement

Everything in this section is measured, not asserted. The experiments
are recorded in `process-docs/skills/entitlement-build-mounts.md`.

### The mount target is `/run/secrets/`, forced by control flow

`rhsm/config.py` in the base image decides it is running in a
container by testing `/etc/rhsm-host` and `/etc/pki/entitlement-host`,
two symlinks the base ships pointing into `/run/secrets/`. When the
test passes, it rewrites `ca_cert_dir` and `entitlementCertDir` to the
`-host` paths, and the real paths stop being consulted.

The decisive experiment: with a non-empty decoy at
`/run/secrets/etc-pki-entitlement`, which is the standing condition on
any subscribed build host, a fragment mounting the real paths
(`/etc/pki/entitlement`, `/etc/rhsm`) is silently ignored and the
build fails while the fragment is correct and complete. The real-path
model works in isolation and loses on exactly the hosts most likely to
build RHEL images. `/run/secrets/` is the only target the host cannot
override, so it is the target.

### The minimum viable set

Four files, and the durable statement of the set is by role rather
than by filename: the entitlement certificate, its key, `rhsm.conf`,
and whatever file `repo_ca_cert` in that `rhsm.conf` resolves to. For
a stock CDN-direct configuration that resolution is `redhat-uep.pem`;
Satellite, Katello, and proxied setups change it, and the by-role
statement stays correct where a flat filename list would not.

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
  grep across the base's `site-packages` trees returns zero references
  to it; the file that is consulted is named by the hardcoded default
  `"repo_ca_cert": "%(ca_cert_dir)sredhat-uep.pem"`; and the one code
  path that could have used it, the Candlepin connection's directory
  trust store, is guarded by a not-in-container check and never runs
  during a build.

`redhat.repo` is not needed and not shipped. `rhel-bootc:latest`
(10.2, measured) ships a completely empty `/etc/yum.repos.d/`; the
subscription-manager dnf plugin generates the repo configuration at
build time from the mounted certs, correct serial included. Shipping
`redhat.repo` would also put a file directly in `mount/run/secrets/`,
which makes `run/secrets` itself a mount point and collapses the two
intended mounts into one by the nesting-prune rule. That collapsed
variant passes, and fully masks the host, but it must be a deliberate
choice, never an accident of including one extra file.

The serial in the layout above is a placeholder and must stay one: a
real entitlement serial identifies a subscription, so neither the
exemplar nor any public document carries a real one. The exemplar's
placeholder files use an obviously fake serial such as
`0000000000000000.pem`.

### The confound any reproducer will hit

`containers-common` ships a `mounts.conf` that injects
`/run/secrets` from `/usr/share/rhel/secrets` into every container and
every build step, on every RPM-family builder. On a macOS podman
machine that injection arrives pre-loaded with both CA certificates,
because the machine VM has `subscription-manager-rhsm-certificates`
installed. A naive subtraction test on such a host concludes the CA
files are unnecessary; they are not, the host was supplying them. The
runs behind this document were controlled for it with two negative
controls (nothing mounted must fail; an empty directory mounted over
`/run/secrets` must also fail) and by re-verifying that the passing
results shadowed every path the host could have populated.

### Two diagnostics worth keeping

From the CA isolation run, two facts that separate failure modes which
otherwise present identically:

- Repo generation does not depend on the CA. A populated `redhat.repo`
  together with curl error 77 means the CA file is missing; an empty
  `redhat.repo` means the entitlement certificate is the problem.
- `sslcacert` in the generated output is copied from `repo_ca_cert`
  whether or not the file exists, so its presence in `redhat.repo` is
  not evidence the CA was mounted.

## Why the mandatory pin holds here, twice over

The build-mounts spec pinned mount-carrying fragments for a trust
reason: a movable tag on an artifact that injects trust material is an
invisible substitution point. The empirical work adds a second,
independent argument, and it is the stronger one because it is
mechanical and assumes no adversary.

At build time, `RUN --mount=from=<ref>` resolves against local storage
first and contacts the registry on a miss (measured: with the image
purged, the build printed the pull and proceeded). A tag reference can
therefore be shadowed by stale local content, and that failure is
recorded as having actually happened in this repo's history
(`process-docs/skills/registry-verification.md`). A digest reference
cannot: local storage is content-addressed, so a local hit on
`@sha256:X` is by definition the bytes the registry would have served.
For the one fragment class where stale trust material is the worst
possible substitution, the pin rule closes the hazard as a side
effect of its trust purpose.

The enforcement itself is registry-agnostic and network-free: the
check is a text test on the manifest's own image reference
(`src/validate.rs:159`, `declared.contains("@sha256:")`). `--pin-digests`
does not satisfy it, because the digest must live in the user's own
`image:` line to survive; and port-carrying references pin correctly,
so the rule behaves identically for `localhost:5000` and for a real
private registry. Nothing about the convention relaxes on a test
stand-in.

## The silent-failure cluster

Three ways to be wrong with no signal, all in the path that carries
credentials. They are named here precisely so review can weigh them;
this document proposes no mechanism changes for them beyond what the
convention itself requires.

1. **An empty subdirectory beside a populated one derives nothing,
   silently.** The empty-mount notice fires only when the entire
   `mount/` is fileless (`empty_mount_notice`, `src/mount.rs`). A
   partial exemplar that ships some paths as bare directories quietly
   mounts less than the live fragment does, passes validation, and
   fails at dnf with an error that points at credentials rather than
   at the missing mount. The convention's requirement that placeholder
   files exist at every live path is the exposure surface for this,
   which is why the proposed format text below states the requirement
   as a rule rather than a suggestion.

2. **A decoy at `/run/secrets/etc-pki-entitlement` silently defeats a
   real-path mount.** Measured, and it is the standing condition on
   any subscribed build host, not an exotic one. The chosen
   `/run/secrets/` target avoids it for instance one; any future
   instance whose consumer implements a similar host-redirect must be
   traced the same way before its paths are documented, because the
   failure produces no signal that a mount was ignored.

3. **A stale `target/release` binary emits a Containerfile with no
   mount flags and exits zero.** Observed directly: a binary two days
   older than `src/mount.rs` printed no `mount/` section from
   `inspect` and generated a mount-free Containerfile, which then
   fails much later, at dnf, with a package-not-found error. Whether
   the release binary belongs in the tree at all is a repo hygiene
   question this document leaves open; it is listed because its
   failure lands in the same credential path with the same absence of
   signal.

## Limits, stated as boundaries

Two things the mechanism cannot express. They are boundaries of the
design, not defects discovered later, and this document states them so
the acceptance test in decision 7 has explicit edges.

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
   `facts/` and `syspurpose/`, both of which the base populates. An
   instance whose credential path sits inside a load-bearing directory
   has no clean expression today.

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

> ### Exemplars for fragments that mount credentials
>
> A fragment whose `mount/` carries live credential material
> (entitlement certificates, mirror client certificates, proxy CA
> bundles) is normally published as a pair: a public **exemplar**
> carrying placeholder files, and a live counterpart carrying the real
> material, built wherever that material is available. The convention
> below keeps the two from being mistaken for each other.
>
> An exemplar must ship real files, at exactly the paths the live
> fragment uses. Derivation is presence-based: a mount point exists
> only when a directory under `mount/` directly contains a regular
> file, so a fragment cannot declare a mount target without shipping a
> file at it, and an empty subdirectory beside a populated one derives
> nothing for the empty path, without a notice. Identical paths make
> the pair derive identical targets, so `inspect` shows the same
> `mount/` section for both, and switching between them is a one-line
> `image:` change in the manifest. Placeholder material does not
> authenticate: a composition built from an exemplar fails at the
> package step with an SSL error against the repository that needed
> the real material.
>
> Naming carries the distinction, on both the fragment name and the
> image repository. The exemplar appends `-exemplar` to the live
> fragment's name (`rhel-entitlement-exemplar` beside
> `rhel-entitlement`) and publishes under a repository name with the
> same suffix. The fragment name is what `inspect` and `list` print
> and what the generated Containerfile records in its header and its
> `--mount=...,from=` reference, and a manifest entry pinned by digest
> shows no tag at all, so a tag-only convention would be invisible on
> every surface a review reads. No annotation work is needed: the name
> annotation mirrors `fragment.toml` and carries the suffix with it,
> and the mounts annotation is identical between the pair by
> construction. A fragment name ending in `-exemplar` is reserved by
> this convention to assert that its `mount/` material is placeholder.
>
> Both halves of the pair derive mount points, so both are subject to
> the digest-pinning requirement above. The live half belongs in a
> registry its consumer controls, since pull access to it equals
> possession of the credential.

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
- **OpenShift on-cluster layering was not exercised for this
  convention.** The build-mounts spec records that entitlement
  fragments are unnecessary there; the remaining instances on that
  path ride the one-authfile model and are untested.
- **The proposed format text is not applied.** `docs/fragment-format.md`
  is unchanged by this document; the quoted block above is the
  proposal.
