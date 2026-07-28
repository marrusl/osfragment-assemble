# OCL build environment (OpenShift on-cluster layering)

What the `--ocp` output path actually runs inside. Verified 2026-07-27 against
`openshift/machine-config-operator` and `containers/buildah` sources.

For *which* Containerfile constructs keep bytes out of an image, and the
`from=context` / `from=<image>` / `from=<stage>` distinction, see
`containerfile-layer-semantics.md`. This file covers the environment those
constructs run in.

## The builder is buildah, and you can rely on that

An OCL build is `buildah bud`. Not podman, not BuildKit, not Docker.

It runs as an init container named `image-build` in a Kubernetes Job. The
container image is the MCO image itself, which installs `buildah` from the
RHEL 9 base repos (`Dockerfile`: `dnf install -y buildah fuse-overlayfs cpp`).
The invocation lives in
`pkg/controller/build/buildrequest/assets/buildah-build.sh`:

```
buildah bud --log-level=DEBUG --storage-driver vfs \
  --authfile="$BASE_IMAGE_PULL_CREDS" --tag "$TAG" \
  --file="$build_context/Containerfile" ... "$build_context"
```

Environment, all confirmed in `buildrequest.go`:

- `BUILDAH_ISOLATION=chroot`
- runs as user `build`, uid 1000, via `su -m build`
- `securityContext` is an empty struct: **not privileged**, no added capabilities
- ServiceAccount `machine-os-builder`
- `--storage-driver vfs`, cache volume at `/home/build/.local/share/containers`
- `HOME=/home/build`, build context at `/home/build/context`

Practical consequence: **the OCP path is the one output target where the builder
is known in advance.** Buildah extensions are safe there. They are not
automatically safe on the generic path, where the user picks the builder. That
asymmetry is why the fallback emission mode is scoped to the generic path only.

## Version floor is not a live concern

Buildah gained buildkit-style `--mount=type=bind` in **v1.24.0 (2022-01-26)**;
`from=<stage>` predates that (v1.29.0 carries a *fix* to it). RHEL 9 has shipped
buildah 1.29+ since 9.2 and 1.33+ since 9.4, and OCL itself only appeared in
OCP 4.16. Every OCP release that can run OCL at all is far past the floor.

Do not add version-gating logic for this.

## Credentials: one authfile for everything

A single global `--authfile="$BASE_IMAGE_PULL_CREDS"`, sourced from the
MachineOSConfig's base image pull secret, covers every pull in the build:
the base image, images named in `COPY --from=`, and images named in
`RUN --mount=...,from=`.

**Constraint:** a fragment image must be pullable with the same secret that
pulls the base image. Not new with mounts - buildah routes `COPY --from=<image>`
and `RUN --mount=...,from=<image>` through the same `getImageRootfs` function,
so both have always had this property.

The build pod also gets RHSM certs, entitlements, `yum.repos.d`, and `rpm-gpg`
bind-mounted in via `--volume` when the corresponding secrets exist, which is
what makes subscription-backed `dnf` work inside a fragment hook.

## MCO validates the Containerfile but does not restrict flags

`pkg/controller/build/buildrequest/buildrequest.go` validates twice: the user's
`containerFile` alone, and the fully rendered Containerfile.

- Requires at least one `FROM` (regex `^\s*FROM`).
- Parses with `github.com/openshift/imagebuilder/dockerfile/parser`. Instruction
  flags are collected generically into `Flags []string`. **No allowlist, no
  inspection of flag contents.**
- Checks that instructions requiring arguments have them.
- Error handling is deliberately permissive where imagebuilder is stricter than
  buildah: heredoc syntax and unquoted LABEL/ENV values are waved through, with
  a source comment noting "imagebuilder cannot parse them but buildah/podman
  will."

So heredocs are safe, and `--mount` flags are safe. If a future emission is
rejected by MCO, suspect the character cap before suspecting the parser.

## The 4096-character cap is the real OCL constraint

`containerFile` is capped at 4096 characters (`openshift/api`,
`machineconfiguration/v1/types_machineosconfig.go`). This is the binding limit
on fragment count for the OCP path, roughly 6-8 with per-fragment `COPY`
emission. A single `RUN --mount` per fragment costs fewer characters than a
`COPY` plus a separate `RUN`; measure the new ceiling rather than assuming one.

MachineOSConfig has graduated to `v1`. The `v1alpha1` type is gone, and
`imageBuilderType` accepts only `Job`.

## The user does not control the build context

`buildah-build.sh` creates `/home/build/context` and copies in the Containerfile,
`machineconfig/machineconfig.json.gz`, and the CA bundle, then passes it to
`buildah bud`. A build context exists, but the only user-supplied input reaching
it is the `containerFile` string itself, delivered via ConfigMap.

Anything a fragment needs must therefore arrive as a registry image, never as a
context file.
