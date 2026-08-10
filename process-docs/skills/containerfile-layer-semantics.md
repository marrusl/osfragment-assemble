# Containerfile layer semantics: what actually keeps bytes out of an image

Correctness requirements that are easy to get wrong when emitting Containerfile
instructions from the generator.

## `COPY` then `RUN rm -rf` does not remove anything

```dockerfile
COPY --from=<ref> /fragment/hook/ /tmp/frag-hook/
RUN /tmp/frag-hook/configure.sh && rm -rf /tmp/frag-hook
```

This is **filesystem-correct but not layer-correct**. The `rm -rf` writes a whiteout
in the `RUN` layer. The files are absent from the mounted filesystem and absent
from what a deployed bootc node sees, because ostree/composefs only surfaces
what is in the commit. The bytes themselves remain in the `COPY` layer and come
back out with `podman save` or `skopeo copy`.

Costs: pull size at fleet scale, and disclosure of a fragment's implementation
logic (matters for security-oriented fragments).

## There is no "fold the `rm` into the `COPY`" fix

`COPY` is a build directive, not a shell command. It cannot be `&&`-chained with
`rm -rf`, and it always produces its own layer. Any plan that reads "put the
`COPY` and the cleanup in the same instruction" is describing an operation that
does not exist. This mistake reached both a spec and a topic note before it was
caught, so it is worth stating flatly.

## What does keep bytes out

- `RUN --mount=type=bind,from=<source>` is one instruction and one layer.
  Nothing is committed, so nothing needs cleaning up. This is the standard
  pattern for build-time-only content.
- A multi-stage build where the disposable stage is never `COPY`ed from. Note
  this still needs `RUN --mount=type=bind,from=<stage>` to use the content in
  the final stage, so it does not help where `RUN --mount` itself is the thing
  in doubt.
- `--squash`. Lossy, non-standard, and it destroys the readability of a
  generated Containerfile. Not used here.

## A multi-source `COPY` into a directory copies contents, not directories

Fragment authoring, not generator emission, but the same failure family and it
sat wrong in both the README and `docs/fragment-format.md` for a long time:

```dockerfile
# Wrong: produces /fragment/configure.sh and /fragment/etc/...
COPY fragment.toml tree/ hook/ /fragment/
```

With a directory destination, `COPY` copies the *contents* of each source
directory, so `tree/` and `hook/` are flattened into `/fragment/` and neither
`/fragment/tree/` nor `/fragment/hook/` exists. The loader keys on exactly
those two prefixes, so a fragment built this way reports no tree and no hooks
while building and pushing without error. Each directory needs its own `COPY`
with an explicit destination:

```dockerfile
COPY fragment.toml /fragment/
COPY tree/ /fragment/tree/
COPY hook/ /fragment/hook/
```

Verified 2026-07-31 by building both forms and listing the layer contents.
Both documents now show the correct form.

## `--mount=type=bind,from=` has three sources, and they are not interchangeable

- `from=context`: needs a build context holding the user's files.
- `from=<registry image ref>`: the builder pulls it. No build context involved.
- `from=<named build stage>`: resolves inside the Containerfile.

Omitting `from=` is a fourth case: the mount `source=` resolves against the
build context, the same place a bare `COPY <src>` reads from. Self-contained
mode emits this form (`source=fragments/<name>/hook`) because it materializes
the fragments into the build context itself, so there is no image or stage left
to name. Default and OCP modes emit the second and third. When reasoning about
whether a bind
mount works in some environment, check which form is at issue. The common
objection that on-cluster OpenShift builds "have no build context" applies to
`from=context` only. An on-cluster build does have a build context; MCO supplies
the Containerfile plus a context directory. What it lacks is a build context the
**user** controls.

Registry resolution in the MCO build pod is already a dependency of the OCP
output: it emits `COPY --from=<registry ref>` per fragment and, under
`--pin-digests`, `FROM <fragment ref> AS frag-<name>` stages.

## `RUN --mount=type=bind,from=<image>` works in an on-cluster OpenShift build

Verified 2026-07-27. MCO's own on-cluster-build Containerfile template
(`openshift/machine-config-operator`,
`pkg/controller/build/buildrequest/assets/Containerfile.on-cluster-build-template`)
ships this instruction with `from=` set to a registry image pullspec, rendered
into the same Containerfile that carries the user's `containerFile`, passed
through the same validator, and run by the same `buildah bud` invocation in the
same pod. Buildah resolves `COPY --from=<image>` and
`RUN --mount=type=bind,from=<image>` through the same image-rootfs path and the
same `--authfile`, so the mount form needs nothing the `COPY` form does not.

Practical notes:

- Mirror MCO's options, `bind-propagation=rshared,z`, rather than the bare form.
  This applies to `from=<image>` mounts. `bind-propagation` is inert for a
  static build-context source, so self-contained mode drops it and keeps only
  `z` (the SELinux relabel, which still matters).
- The fragment image must be pullable with the same pull secret that pulls the
  base image. This is a pre-existing property of the `COPY --from` path, not
  something mounts introduce.
- MCO's production use mounts `source=/`. Mounting a subdirectory is standard
  buildah, but it is one step past what the production evidence covers.
