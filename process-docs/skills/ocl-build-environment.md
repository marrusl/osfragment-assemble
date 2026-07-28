# OCL build environment (OpenShift on-cluster layering)

What the `--ocp` output path actually runs inside. Verified 2026-07-27 against
`openshift/machine-config-operator` and `containers/buildah` sources.

## The builder is buildah, and you can rely on that

An OCL build is `buildah bud`. Not podman, not BuildKit, not Docker.

The build runs as an init container named `image-build` in a Kubernetes Job. The
container image is the MCO image itself, which installs `buildah` from the
RHEL 9 base repos. The invocation lives in
`pkg/controller/build/buildrequest/assets/buildah-build.sh`:

```
buildah bud --log-level=DEBUG --storage-driver vfs \
  --authfile="$BASE_IMAGE_PULL_CREDS" --tag "$TAG" \
  --file="$build_context/Containerfile" ... "$build_context"
```

Environment: `BUILDAH_ISOLATION=chroot`, running as user `build` (uid 1000),
empty `securityContext` (not privileged), `--storage-driver vfs`,
ServiceAccount `machine-os-builder`.

Practical consequence: the OCP path is the one output target where the builder
is known in advance. Buildah extensions are safe there. They are not
automatically safe on the generic path, where the user picks the builder.

## `RUN --mount=type=bind,from=<registry-image>` works. Use it.

This is the important one, because the opposite was assumed for a while.

MCO ships this exact instruction in its own on-cluster-build Containerfile
template (`assets/Containerfile.on-cluster-build-template`, line 25) to mount
the extensions image:

```dockerfile
RUN --mount=type=bind,from={{.ExtensionsImage}},source=/,target=/tmp/mco-extensions/os-extensions-content,bind-propagation=rshared,z \
    bash <<'EOF'
```

`{{.ExtensionsImage}}` is a registry pullspec, not a build stage. The user's
`containerFile` is rendered into this same template and validated by the same
validator, so anything MCO does here is available to us.

**Do not emit a `COPY`-based fallback for `--ocp` output.** A `COPY` cannot
avoid leaving its bytes in a layer (see below), so a fallback there ships hook
bytes in every image for no benefit.

Mirror MCO's options rather than emitting the bare form:
`bind-propagation=rshared,z`. The `z` is an SELinux relabel and MCO presumably
added both deliberately.

## Three `from=` sources, do not conflate them

`RUN --mount=type=bind` accepts `from=` naming three different things, and
buildah resolves them in this order (`imagebuildah/stage_executor.go`,
`stageMountPoints`):

1. an additional build context (`--build-context name=...`) - MCO passes none
2. an earlier build stage in the same Containerfile
3. otherwise, an image name, pulled if not in local storage

Only form 1 needs a build context. The claim "OCL has no build context" is both
inaccurate and irrelevant to us:

- It is inaccurate: `buildah-build.sh` creates `/home/build/context` and passes
  it to `buildah bud`. The real constraint is that **the user cannot put files
  into it** - the only user input is the `containerFile` string.
- It is irrelevant: this tool emits forms 2 and 3, which never touch the build
  context.

## Credentials: one authfile for everything

There is a single global `--authfile="$BASE_IMAGE_PULL_CREDS"`, sourced from the
MachineOSConfig's base image pull secret. It covers every pull in the build,
including images named in `COPY --from=` and `RUN --mount=...,from=`.

**Constraint:** a fragment image must be pullable with the same secret that
pulls the base image. This is not new with mounts - buildah routes
`COPY --from=<image>` and `RUN --mount=...,from=<image>` through the same
`getImageRootfs` function, so both have always had this property.

## MCO validates the Containerfile, but does not restrict flags

`pkg/controller/build/buildrequest/buildrequest.go` validates twice: the user's
`containerFile` alone, and the fully rendered Containerfile.

- Requires at least one `FROM` (regex `^\s*FROM`).
- Parses with `github.com/openshift/imagebuilder/dockerfile/parser`. Instruction
  flags are collected generically into `Flags []string`. **There is no allowlist
  or inspection of flag contents.**
- Checks that instructions requiring arguments have them.
- Error handling is deliberately permissive where imagebuilder is stricter than
  buildah: heredoc syntax and unquoted LABEL/ENV values are waved through, with
  a source comment noting "imagebuilder cannot parse them but buildah/podman
  will."

So heredocs are safe, and `--mount` flags are safe.

## The 4096-character cap is the real OCL constraint

`containerFile` is capped at 4096 characters (`openshift/api`,
`machineconfiguration/v1/types_machineosconfig.go`). This is the binding limit
on how many fragments the OCP path can carry, roughly 6-8 with per-fragment
`COPY` emission. Switching to a single `RUN --mount` per fragment reduces the
per-fragment cost; measure it rather than assuming a specific new ceiling.

Also: MachineOSConfig has graduated to `v1`. The `v1alpha1` type is gone, and
`imageBuilderType` accepts only `Job`.

## Why `COPY` then `rm` never works

Worth restating because it is the bug that started this. `COPY` is a build
directive and cannot be combined with a shell `rm` in one instruction, so it
always lands its own layer. A later `RUN ... && rm -rf` only writes a whiteout;
the bytes remain in the earlier layer and are recoverable with `podman save`.

`RUN --mount` is the only construct that keeps build-input bytes out of the
image. This applies to hooks and any other build input. Content under `tree/` is
payload and should still be copied in, because it is the deliverable.
