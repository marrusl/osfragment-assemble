# Multi-Arch Example Fragments

Status: Proposed. This is a publication design. It changes no code in the
Rust tool. Its subject is publishing the example fragments as
`aarch64`+`amd64` multi-arch manifest lists, and it is the first real
exercise of the 2026-08-04 ruling that architecture is OCI's problem end
to end: skopeo and podman resolve image indexes, and the tool carries no
platform code on purpose. This design verifies that ruling holds and
scopes the pipeline work that follows from it.

Grounding base: the behavioral claims below are read from the OCI image
specification, the `containers/skopeo` and `containers/common` source, and
the `podman`/`buildah` manual pages, all verified 2026-08-09. Where a claim
is a property of the tool, it is read from this repository's source. Each
load-bearing claim is stated with its source rather than asserted.

## What this settles

Every example fragment is published today as a single `arm64` image, so the
position that the tool inherits multi-architecture support from OCI is
untested (`docs/roadmap.md`). This design settles that the position holds,
that publishing multi-arch is a pipeline change with zero tool changes, and
what the concrete pipeline is.

## Scope

Publish all ten example fragments as `linux/amd64`+`linux/arm64` manifest
lists to `quay.io/marrusl2/fragments/`, each at its existing tag (the tag
equals the `fragment.toml` version). Assemble-time behavior is unchanged.

The `rhel-entitlement-example` fragment is not among the ten: it is governed
by the secret-bearing mount design, is digest-pinned by rule, and carries
arch-neutral placeholder content, so it inherits the same story if it is
ever published multi-arch, under that spec rather than this one.

## Grounding: why this is pipeline-only

Five verified facts carry the design.

1. **A from-scratch, text-only image still carries an architecture.** The
   OCI image config makes `architecture` and `os` REQUIRED, so every built
   image is stamped, even one built `FROM scratch` with only `COPY` of text.
   The layers are arch-neutral filesystem changesets; the config, not the
   layers, carries the arch. So for a config-only fragment the layer bytes
   are identical across arches and only the config differs.

2. **There is no true OCI noarch.** The image-index `platform` object has no
   wildcard value. The `unknown/unknown` token is a BuildKit convention for
   attestation manifests whose stated purpose is to keep runtimes from
   selecting them: a do-not-select marker, the opposite of noarch. An
   index whose only entry is `unknown/unknown` matches no real `--platform`
   request and fails resolution. Noarch is a trap, not an option.

3. **A manifest list over arch-neutral content is near-free.** Because the
   layers are identical across arches, both per-arch manifests reference the
   same layer blob digest; the registry stores and uploads that blob once.
   The added bytes are the small per-arch config JSON, two manifest JSONs,
   and one index JSON. This dedup holds only when the layer blobs are
   byte-identical, which a single multi-platform build invocation produces
   and two separate builds can defeat (gzip metadata differs); see change 1.

4. **`COPY --from` and `FROM` resolve the build's target platform.** With
   `--platform linux/amd64`, buildah pulls every referenced image, base
   stages and `COPY --from` / `RUN --mount` sources alike, for that
   platform, and selects the `amd64` instance from a manifest list. This is
   the exact resolution the tool delegates.

5. **`--pin-digests` pins the index digest, so pinning and multi-arch
   coexist.** `resolve_digest` (`src/loader.rs`) runs `skopeo inspect
   --format '{{.Digest}}'`, which computes the digest of the top-level
   manifest. For a manifest list that is the index digest, not a per-arch
   instance digest (read from `containers/skopeo` `cmd/skopeo/inspect.go`:
   the top-level unparsed instance is fetched and its bytes are digested).
   A reference pinned to the index still resolves per platform at build time.

## Fragment inventory

Eight fragments carry arch-neutral content and build identically for both
arches. Two carry a genuinely per-arch binary and must build per arch.

| Fragment | Tag | Content | Multi-arch build |
| --- | --- | --- | --- |
| epel | 10 | repo + GPG key (`$basearch`) | single invocation |
| tailscale | 1.82.0 | repo + GPG key + preset (`$basearch`) | single invocation |
| grafana | 11.0 | repo + GPG key (no arch in URL) | single invocation |
| postgresql | 17 | repo (`$basearch`) + both GPG keys shipped | single invocation |
| hashicorp | 1.0 | repo + GPG key (`$basearch`) | single invocation |
| cis-hardening | 2.1 | sysctl/tmpfiles config + hook | single invocation |
| node-exporter | 1.8.0 | repo + GPG key + preset (`$basearch`) | single invocation |
| nginx | 1.26 | repo + GPG key + preset (`$basearch`) | single invocation |
| awscli-zip | 2.36.16 | per-arch installer zip in `hooks/` | per-arch build |
| nvidia-driver-run | 610.57.04 | per-arch `.run` installer in `hooks/` | per-arch build |

PostgreSQL looks arch-specific and is not. Its `.repo` uses `$basearch`,
which dnf expands at install time inside the assembled build, and it already
ships both `PGDG-RPM-GPG-KEY-RHEL` and `PGDG-RPM-GPG-KEY-AARCH64-RHEL` with
each repo section referencing both, so the right key is present whichever
arch installs. The content does not diverge.

## The three changes

### 1. `hack/build-fragments.sh`

A new script that builds and pushes each example fragment as a manifest
list. For the eight arch-neutral fragments, a single invocation per
fragment, which is required so the identical layer blob dedupes (two
separate per-arch builds can emit non-identical gzip and defeat it):

```
podman build --platform linux/amd64,linux/arm64 \
  --manifest quay.io/marrusl2/fragments/<name>:<tag> \
  -f examples/fragments/<name>/Containerfile.fragment \
  examples/fragments/<name>
podman manifest push --all quay.io/marrusl2/fragments/<name>:<tag>
```

The two arch-specific fragments cannot use the single-invocation form,
because one build context carries one arch's binary and would mislabel it.
The script fetches each arch's binary in turn and builds per arch, then
assembles the list:

```
./examples/fragments/<name>/fetch-*.sh x86_64
podman build --platform linux/amd64 -t <name>-amd64 examples/fragments/<name>
./examples/fragments/<name>/fetch-*.sh aarch64
podman build --platform linux/arm64 -t <name>-arm64 examples/fragments/<name>
podman manifest create quay.io/marrusl2/fragments/<name>:<tag>
podman manifest add quay.io/marrusl2/fragments/<name>:<tag> containers-storage:<name>-amd64
podman manifest add quay.io/marrusl2/fragments/<name>:<tag> containers-storage:<name>-arm64
podman manifest push --all quay.io/marrusl2/fragments/<name>:<tag>
```

No fragment build runs any foreign-arch code: every `Containerfile.fragment`
is `FROM scratch` with `COPY` only, so building either arch needs no
emulation. Emulation matters only at assemble time (see below).

### 2. `nvidia-driver-run` gains its `amd64` checksum

`examples/fragments/nvidia-driver-run/fetch-run-installer.sh` records only
the `aarch64` installer digest today (`sha256_for_arch()` returns `1` for
`x86_64`), so it refuses to fetch the `amd64` installer. Add the `x86_64`
`.run` digest so the `amd64` variant can build. `awscli-zip` already records
both checksums and needs no change.

### 3. Doc note on digest pinning and complete lists

Add a short note to the fragment-authoring documentation
(`docs/fragment-format.md`), stating two rules a multi-arch consumer must
follow:

- When pinning a fragment by digest in a manifest, reference the index
  digest (what `skopeo inspect '{{.Digest}}'` returns for a manifest list),
  never a per-arch instance digest. An instance digest points at one arch
  and defeats platform resolution.
- A published manifest list must include every target architecture. A list
  that is missing the build's target arch is a hard error at assemble time,
  which is stricter than a single-arch image: a single-arch image used as a
  `COPY --from` source into a foreign-arch build only warns and proceeds
  (`containers/common` `libimage/pull.go` warns on platform mismatch and
  does not fail), whereas an incomplete list stops the build.

## Assemble-time behavior

Unchanged. The generator emits the same Containerfile; the target platform
is chosen by the consumer's `podman build --platform`, and OCI resolution
selects the matching fragment instance. Building an assembled image for a
foreign arch runs the package step and any hooks under emulation, which the
`podman machine` VM provides. Cost lands on the consumer at assemble time,
dominated by any hook that compiles for the foreign arch (the NVIDIA driver
fragment is the demanding case); the tool adds nothing to it.

## Verification

1. **Emulation is live:** `podman run --rm --platform linux/amd64
   quay.io/centos-bootc/centos-bootc:stream10 uname -m` prints `x86_64`.
2. **Each list carries both platforms:** after push, `skopeo inspect --raw
   docker://quay.io/marrusl2/fragments/<name>:<tag>` shows an index with
   `linux/amd64` and `linux/arm64` entries.
3. **End to end:** assemble `examples/manifests/full.yaml` with `podman
   build --platform linux/amd64`, confirm the `amd64` fragment variants are
   pulled and the image builds, and confirm the native `arm64` path still
   builds.

## Out of scope

Recorded so the boundary is explicit; none is built here.

- CI-driven publishing. The script is run by hand; wiring it into CI is a
  later decision.
- Multi-arch `--self-contained` output. That mode materializes one arch's
  bytes into a local build context and is single-arch by construction;
  making it multi-arch is a separate design, not a config flag.
- Architectures beyond `amd64` and `arm64`. No `ppc64le` or `s390x`.
