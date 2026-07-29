# Skills Index

Skills files capture non-obvious patterns, workarounds, and correctness requirements discovered during development. Read these before working on related code.

## Available Skills

- **[containerfile-layer-semantics.md](containerfile-layer-semantics.md)** — What actually keeps bytes out of an image: why `COPY` then `RUN rm -rf` doesn't work, why `RUN --mount=type=bind` does, the three sources for `from=`, and verification that bind mounts work in OpenShift on-cluster builds.

- **[ocl-build-environment.md](ocl-build-environment.md)** — The OpenShift on-cluster layering build environment: buildah as the builder, credential handling, Containerfile validation, the 4096-character cap, and why the user doesn't control the build context.
