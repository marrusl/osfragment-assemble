# Entitlement Fragment

## Problem

Building a RHEL bootc image on a non-RHEL host (Fedora, CentOS Stream,
Ubuntu, macOS, CI runner) fails at `dnf install` for any RHEL-only package.
The builder has no entitlement certs at `/etc/pki/entitlement/` for podman
to auto-mount. Today's workarounds (manual cert transfer, build secrets,
`SMDEV_CONTAINER_OFF`) are all manual and per-build.

## Proposed mechanism

A fragment whose `hooks/` directory carries the consumer's entitlement
certificates (the `.pem` key pair). The generator attaches this fragment's
hook material as a `--mount=type=bind` on the batched `dnf install` RUN
line, targeting the path the fragment declares.

Because hooks are bind-mounted and never COPY'd, the certs never touch
the image layers. The consumer publishes their certs as a fragment to
their private registry, pins it in their manifest, and every other
fragment's `packages.required` just works against RHEL repos.

The new thing: today, hook material is only mounted on the hook's own
`RUN` instruction (at `/frag-hooks`). This feature mounts hook material
on a *different* step -- specifically, the batched package install.

## fragment.toml

```toml
[fragment]
name = "rhel-entitlement"
version = "1.0"
description = "RHEL entitlement certificates for build-time repo access"

[build-mounts]
packages = "/etc/pki/entitlement"
```

`[build-mounts]` is a new section. The key names a generator phase
(`packages`); the value is the mount target path. The fragment's `hooks/`
directory is the implicit mount source. No `[packages]`, no `[provides]`,
no entrypoint -- this fragment exists solely to inject credentials into
the package step.

## Generated Containerfile sketch

Before (no entitlement fragment):

```dockerfile
RUN dnf install -y \
        some-rhel-package \
    && dnf clean all \
    && rm -rf /var/cache/dnf ...
```

After (entitlement fragment in manifest):

```dockerfile
RUN --mount=type=bind,from=frag-rhel-entitlement,source=/fragment/hooks,target=/etc/pki/entitlement,z \
    dnf install -y \
        some-rhel-package \
    && dnf clean all \
    && rm -rf /var/cache/dnf ...
```

The mount attaches to the existing batched `RUN` line. No new `RUN`
instruction. The certs are visible to dnf during install and vanish
when the layer commits.

## Constraints

- The entitlement fragment image must live in a private registry.
  Publishing certs to a public registry leaks credentials.
- Only `hooks/` is mountable (existing invariant). No new mount source
  directories.
- Self-contained mode uses `source=fragments/<name>/hooks` instead of
  `from=<stage>` -- same pattern as existing hook mounts.
- A fragment with `[build-mounts]` and no entrypoint should not emit
  its own hook `RUN` instruction. The hook material is consumed by the
  mount, not executed.

## Open questions

1. **Phase vocabulary.** Is `packages` the right (only) key, or should
   `build-mounts` accept arbitrary phase names for future use?
2. **Multiple build-mount fragments.** If two fragments declare
   `[build-mounts] packages = "/some/path"`, the generator needs to
   emit multiple `--mount` flags on the same `RUN`. Straightforward,
   but needs a collision rule for overlapping target paths.
3. **Validation.** Should the generator warn or error if the mount
   target path is unusual (not `/etc/pki/entitlement`), or stay generic?
4. **OCP mode.** The MCO Containerfile has its own package step. Does
   the `--mount` survive the `FROM configs AS final` rewrite, or does
   OCP need a different entitlement delivery path?
