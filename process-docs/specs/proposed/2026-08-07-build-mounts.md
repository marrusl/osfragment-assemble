# Build Mounts

## Problem

A package install step often needs material that must exist during the
build and must not exist in the built image. The defining case is
credentials for package sources: RHEL entitlement certificates, SUSE SCC
credentials, mTLS client certificates for Artifactory or Nexus mirrors,
CA bundles for TLS-intercepting corporate proxies.

Today this works one of two ways. Host-coupled engine magic: podman
auto-mounts entitlement certificates into build containers, but only on
a subscribed RHEL host, and SLE hosts inject SCC credentials for
container-suseconnect the same way. Or manual per-build secret plumbing,
which is not distributable, not pinnable, and different for every
builder and CI system.

The founding instance: building a RHEL bootc image on a non-RHEL host
(Fedora, CentOS Stream, Ubuntu, macOS, CI runner) fails at dnf for any
RHEL-only package. Nothing in the mechanism below is specific to any
distro. It replaces host-coupled engine magic with an artifact-coupled
declared mount: works from any host, pinnable, versioned, and shippable
by a second author.

## Mechanism

A fragment may carry a `mount/` directory, a sibling of `tree/` and
`hooks/`. Its subtree mirrors target paths, the same convention `tree/`
uses: material at `mount/etc/pki/entitlement/cert.pem` is visible at
`/etc/pki/entitlement/cert.pem` during the package install step, and
appears nowhere in the built image.

Detection is presence-based, exactly like repo files. There is no new
`fragment.toml` section, no phase vocabulary, and no new fragment kind.

The generator's package phase today collects repo files from fragments
and copies them into place ahead of the batched dnf RUN. Build mounts
extend that same phase with a second verb: (a) copy the config that
belongs in the image, and (b) mount the material that must not persist.
Both attach to the step they serve.

## Fragment layout

```
rhel-entitlement/
  fragment.toml          # [fragment] metadata only
  mount/
    etc/pki/entitlement/
      cert.pem
      key.pem
```

A fragment combining a repo definition with its access credential ships
as one pinnable unit:

```
internal-mirror/
  fragment.toml
  tree/
    etc/yum.repos.d/internal.repo   # ships in the image
  mount/
    etc/pki/tls/mirror/
      client.pem                    # exists only during dnf
      client-key.pem
```

A fragment with only metadata and `mount/` is valid. Because `mount/`
is its own directory, the hooks entrypoint contract is untouched:
`hooks/`, when non-empty, still requires an executable entrypoint, and a
pure mount fragment has neither.

## Generated Containerfile

The generator adds one `--mount` flag per mount point to the existing
batched dnf RUN:

```dockerfile
RUN --mount=type=bind,from=quay.io/acme/rhel-entitlement@sha256:abc...,source=/fragment/mount/etc/pki/entitlement,target=/etc/pki/entitlement,ro,z \
    dnf install -y \
        some-package \
    && dnf clean all \
    && rm -rf /var/cache/dnf ...
```

No new stage, no new RUN instruction, no new layer. Build-mount
references are always emitted inline, never as named stages, including
under `--pin-digests`.

Mount point derivation: bind mounts shadow their target directory,
unlike `COPY tree/ /`, which merges. The generator therefore collects
every directory under `mount/` that directly contains a file, then
drops any collected directory nested inside another collected
directory. Each survivor becomes one `--mount` flag.
`mount/etc/rhsm/rhsm.conf` plus `mount/etc/rhsm/ca/cert.pem` yields a
single mount of `etc/rhsm`; material in two unrelated locations yields
two flags.

Self-contained mode emits `source=fragments/<name>/mount/<path>` with
no `from=`, the same pattern hooks use. This materializes mount content
into the build context and its sibling tar.gz; generation prints a
notice when that happens.

## Digest pinning

A build-mount fragment referenced without a digest is a generation
error. A movable tag on an artifact that injects trust material into
the package step is an invisible substitution point: whoever can move
the tag can swap a CA bundle or credential and redirect the entire
package fetch. Pinning by digest is the verifiable control. This is the
one deliberate asymmetry with ordinary fragments, which pin only under
`--pin-digests`.

## Validation

- Two fragments mounting overlapping targets is a generation error
  naming both fragments. First-wins on credentials produces silent
  authentication mysteries; the tool refuses instead.
- A mount target that collides with a path the generator itself writes
  is the same error.
- Target paths carry no other policy. There is no expected-path list
  and no warning for unusual paths. Path taxonomy is where distro
  assumptions would creep in; the mechanism stays neutral.

## Visibility

Detected mount targets are stamped into an OCI annotation at fragment
publish time, joining the existing `provides.repos` pattern. `list` and
inspect can then surface "this fragment mounts material into the
package step, at these paths" from registry metadata, without pulling
the fragment.

## Security posture

Mount material lives in the fragment's own layers. At rest it exists in
the registry and in the containers-storage of every builder that pulls
it: pull access equals possession. The intended custody model is a
private registry with access control, and the digest pin ensures the
material that arrives is the material that was pinned. Nothing from
`mount/` enters the built image's layers.

The mechanism fits revocable, expiring, org-scoped access material:
registration and entitlement certificates, mirror client certificates,
CA bundles. Long-lived signing keys are a different category; see open
question 1.

## Scope

Version 1 mounts onto the batched package RUN only. Hook RUN
instructions do not receive build mounts, which includes a hook's own
package installs: a hook that installs toolchain packages from an
entitled source on a foreign host will not see the mounts. The recorded
extension path for hook consumers is manifest-level wiring, where the
consumer names which fragment's hook receives which mount, keeping
exposure a composition decision.

Two candidate uses were considered and declined:

- **Kernel-devel snapshot fragments.** When a base image's kernel
  packages are absent from its own repos, the build fails loudly naming
  the stale base, and no out-of-band package source papers over it. A
  snapshot fragment would convert that loud failure into quietly forked
  package provenance.
- **Ephemeral toolchain mounts.** RPM toolchains are not relocatable
  (compiler internals resolve absolute paths). The existing rule,
  install and remove within the hook's own RUN, already achieves zero
  persistent bytes.

## Open questions

1. **Long-lived key material.** Should module-signing keys (Secure Boot
   MOK) ever ship as build-mount fragments in a private registry, or
   only via `--mount=type=secret` plumbing that a fragment declares but
   never carries? Both positions have been argued in full and are under
   review, including a hybrid where the fragment carries an encrypted
   key and a build secret carries the passphrase. The resolution
   belongs to the future manifest-wiring work. Version 1 documents the
   at-rest custody model above and takes no position on keys.
2. **OCP path.** The emitted mount uses the form verified against
   on-cluster builds (`RUN --mount=type=bind,from=<registry-image>`
   under buildah). The open residue is credential plumbing: which pull
   secret the on-cluster build uses to resolve a `from=` reference in a
   second private registry, and whether MachineOSConfig can carry one.
   Needs confirmation from the MCO side.
