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

Build mounts exist so package acquisition can authenticate:
entitlements, CA bundles, mirror client certificates. For one
credential in one pipeline, `podman build --secret` is the right tool;
build mounts pay off when credentials need to be distributed, pinned,
and composed. The mechanism is not a secrets manager and takes no
position on key custody; users who need signed artifacts sign
downstream, in builds they own.

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

Build mounts are emitted `ro,z`. The existing hook mounts emit `z`
without `ro`; the asymmetry is deliberate for now, because hooks are
executed material with an established contract and build mounts are
data. The `z` option applies a shared container SELinux label to the
fragment's content in local storage.

Mount point derivation: bind mounts shadow their target directory,
unlike `COPY tree/ /`, which merges. For the duration of the RUN, a
bind mount hides whatever the target directory already contained; only
the mounted material is visible there. The generator collects every
directory under `mount/` that directly contains a file, then drops any
collected directory nested inside another collected directory. Each
survivor becomes one `--mount` flag. `mount/etc/rhsm/rhsm.conf` plus
`mount/etc/rhsm/ca/cert.pem` yields a single mount of `etc/rhsm`;
material in two unrelated locations yields two flags.

Two edges of the derivation rule are pinned down. A regular file
directly under `mount/` is a generation error: it would derive a mount
onto `/`, and the pruning step would then drop every other mount as
nested inside it. A `mount/` directory containing no regular files
derives no mounts and produces a generation-time notice naming the
fragment: an empty `mount/` is almost always an authoring mistake, and
silence would hide it.

## Self-contained mode

Self-contained output carries no registry references by design: no
fragment registry reference appears anywhere in the output, comments
included. Mount material therefore cannot arrive as an inline `from=`
reference. It would have to be materialized into the build context and
mounted as `source=fragments/<name>/mount/<path>` with no `from=`, the
same pattern hooks use, and that puts credential material on disk in
the context directory and in its sibling tar.gz.

That is a custody change, so it is gated rather than noticed. When any
composed fragment carries `mount/`, `--self-contained` fails at
generation with an error naming those fragments and the exact paths
that would land on disk, and naming the flag that proceeds:
`--materialize-mounts`. With the flag given, `mount/` subtrees in the
output are written owner-only, directories at 0700, an explicit
exception to the output directory's normally-readable handoff
contract. The sibling tar.gz preserves those modes.

Git does not record file modes, so the owner-only protection does not
survive a commit: a context containing mount material is for direct
handoff, not for committing to a repository. Both this spec and the
`--materialize-mounts` error text say so.

The digest anchor moves with the references. Because the output names
no registries, the digest pin for a build-mount fragment is not
visible in the generated context; it lives in the manifest's own image
reference, and materialization still pulls digest-verified content.

## Digest pinning

A build-mount fragment referenced without a digest is a generation
error. A movable tag on an artifact that injects trust material into
the package step is an invisible substitution point: whoever can move
the tag can swap a CA bundle or credential and redirect the entire
package fetch. Pinning by digest is the verifiable control. This is the
one deliberate asymmetry with ordinary fragments, which pin only under
`--pin-digests`.

The pin is checked against the manifest's image reference. The digest
lives in the user's own ref, so it survives regardless of
`--pin-digests`, and no per-fragment retention machinery is needed.

One emission consequence: under `--pin-digests` the generator emits a
named stage per fragment for readability, and a fragment consisting of
metadata and `mount/` alone is excluded from that loop. Build-mount
references are always inline, so a stage for a pure mount fragment
would be consumed by nothing and would spend characters against the
4096-character MachineOSConfig content limit for no reader.

## Validation

- Overlap is prefix-based: two mount targets collide when either equals
  or is an ancestor of the other. Two fragments mounting colliding
  targets is a generation error naming both fragments and the colliding
  path. First-wins on credentials produces silent authentication
  mysteries; the tool refuses instead.
- The same prefix rule applies against paths the generator itself
  writes. The package phase copies repo files into `/etc/yum.repos.d`
  and GPG keys into `/etc/pki/rpm-gpg` ahead of the dnf RUN, and a
  mount target that equals or contains one of those paths would shadow
  that material during exactly the RUN that needs it: `mount/etc/pki`
  would hide `/etc/pki/rpm-gpg` for the whole package step. This
  collision is a generation error naming the fragment, the path, and
  the generator phase that owns the path.
- Error messages follow the loader's existing contract: name the
  fragment, state the rule, give the fix. The unpinned-reference error
  additionally shows how to obtain a digest, for example
  `skopeo inspect`.
- `mount/` inherits the loader's tar-entry rules: symlinks and
  hardlinks are rejected. The shared entry validation already runs on
  every fragment layer entry, so this is documentation of existing
  enforcement, not new behavior.
- Target paths carry no other policy. There is no expected-path list
  and no warning for unusual paths. Path taxonomy is where distro
  assumptions would creep in; the mechanism stays neutral.

## Visibility

Mount targets can ride in an OCI annotation, joining the existing
pattern in which annotations cache fragment metadata for `list`. The
tool has no publish step; like every existing annotation, a mount
annotation is hand-authored by the fragment author, passed as
`--annotation` on their own `podman build`. The recipe: run generation
or a dry run against the fragment locally to see the derived targets,
then annotate at build:

```bash
podman build \
  --annotation 'com.github.marrusl.osfragment.mounts=["/etc/pki/entitlement"]' \
  -f Containerfile.fragment -t quay.io/acme/rhel-entitlement:1.0 .
```

The no-pull benefit belongs to `list`, and only when the annotation is
present: `list` can then report "this fragment mounts material into
the package step, at these paths" from registry metadata alone. When
the annotation is absent, the metadata-only path falls back to a full
pull, as it does for any other missing annotation. `inspect` always
pulls, because its contract is to show payload contents.

Existing annotations cache the in-layer `fragment.toml` and reconcile
against it. A mount annotation has no in-layer file to reconcile
against; its counterpart is the derived mount targets themselves. So
whenever generation pulls the layer anyway, it cross-checks the
annotation against the derived targets and warns on drift, with layer
content authoritative, consistent with the existing cache semantics.

## Security posture

Mount material lives in the fragment's own layers. At rest it exists in
the registry, in the containers-storage of every builder that pulls it,
and, under `--materialize-mounts`, in the self-contained context
directory and its sibling tar.gz: pull access equals possession, and a
materialized context is possession on disk. The intended custody model
is a private registry with access control, and the digest pin ensures
the material that arrives is the material that was pinned. Nothing from
`mount/` enters the built image's layers.

During the build, mounts stay attached for the entire batched dnf RUN.
Every rpm scriptlet in that transaction, from any configured
repository, runs as root with the mounted material readable. The trust
boundary is the one the design rationales already draw: fragments are
trusted build code whose hooks run as root, so the genuinely new
exposure is untrusted packages reading mount material during their
install scriptlets. The hook-RUN exclusion bounds that exposure to the
one dnf step.

A mount-target annotation reveals paths to anyone with registry
metadata access. The derivation rule collapses targets to directories,
so the disclosure is directory granularity only.

The mechanism fits revocable, expiring, org-scoped access material:
registration and entitlement certificates, mirror client certificates,
CA bundles.

## OCP path

The emitted mount uses the form verified against on-cluster builds:
`RUN --mount=type=bind,from=<registry-image>` under buildah. Two
constraints are recorded for the on-cluster case.

Entitlement fragments are unnecessary on-cluster. The MCO build pod
already arrives with RHSM certificates, entitlements, `yum.repos.d`,
and `rpm-gpg` mounted in when the corresponding secrets exist, so the
founding use case is already served on that path.

The remaining build-mount uses, corporate CA bundles and mirror client
certificates, ride the one-authfile model: an on-cluster build routes
every pull through the base image pull secret, so a build-mount
fragment must be pullable with that same credential. A
dockerconfigjson carries entries for multiple registries, so this is a
packaging constraint on the secret, not a blocker, and
`spec.baseImagePullSecret` already exists on the generated
MachineOSConfig for pointing the build at the right secret.

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

1. **Git guard for materialized contexts.** Should `--materialize-mounts`
   also drop a `.gitignore` covering `fragments/*/mount/` into the
   generated context? A committed context would then visibly lack its
   mount material, failing loudly at build time on the git path instead
   of leaking credentials silently, at the cost of surprising a user
   who wanted to commit the whole context.
