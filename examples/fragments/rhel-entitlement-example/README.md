# rhel-entitlement-example

A public example of a fragment whose `mount/` carries credential material. It
ships placeholder files at exactly the paths a live RHEL entitlement fragment
uses, so the two derive identical mount points and switching between them is a
one-line change to the `image:` reference in a manifest.

It authenticates nothing. Building a composition against this fragment fails at
the package step.

## Layout

```
mount/run/secrets/etc-pki-entitlement/0000000000000000.pem       placeholder cert
mount/run/secrets/etc-pki-entitlement/0000000000000000-key.pem   placeholder key
mount/run/secrets/rhsm/rhsm.conf                                 stock config
mount/run/secrets/rhsm/ca/redhat-uep.pem                         placeholder CA
```

Two mount points come out of that, `/run/secrets/etc-pki-entitlement` and
`/run/secrets/rhsm`, because `rhsm/ca` is nested inside a directory that already
qualifies. Confirm with `osfragment-assemble inspect .`.

The target is `/run/secrets/` rather than `/etc/pki/entitlement` and `/etc/rhsm`
because the base image ships `/etc/rhsm-host` and `/etc/pki/entitlement-host` as
symlinks into `/run/secrets`, and subscription-manager redirects its own reads to
those `-host` paths as soon as it detects both are populated. On a build host
that is itself registered, `/run/secrets` arrives pre-populated from the host and
a fragment mounting the real paths is ignored without a word.

## Building your own

Replace the four placeholder files with your own entitlement material, taken
from a registered host:

```
/etc/pki/entitlement/<serial>.pem       -> mount/run/secrets/etc-pki-entitlement/
/etc/pki/entitlement/<serial>-key.pem   -> mount/run/secrets/etc-pki-entitlement/
/etc/rhsm/rhsm.conf                     -> mount/run/secrets/rhsm/
whatever repo_ca_cert in that rhsm.conf resolves to
                                        -> mount/run/secrets/rhsm/ca/
```

For a stock CDN-direct configuration that last file is `redhat-uep.pem`.
Satellite and proxied configurations point `repo_ca_cert` somewhere else, so
read it rather than copying the filename.

Then build, and publish to a registry you control. Pull access to the result is
equivalent to possession of the credential.

```bash
podman build -f Containerfile.fragment \
  --annotation com.github.marrusl.osfragment.name=rhel-entitlement \
  --annotation com.github.marrusl.osfragment.version=1 \
  --annotation 'com.github.marrusl.osfragment.mounts=["/run/secrets/etc-pki-entitlement","/run/secrets/rhsm"]' \
  -t <your-registry>/rhel-entitlement:1 .
podman push <your-registry>/rhel-entitlement:1
```

The manifest entry must be pinned by digest, because the fragment derives mount
points. Drop the `-example` suffix from the fragment name and the repository when
the material is real, and add it back for any published example.

## What failure looks like

Building against this example reaches the package step and stops there. The
durable part is that **no repository file is generated at all**: subscription
manager enters container mode, reads a certificate that carries no entitlement,
and writes nothing to `/etc/yum.repos.d/`.

What dnf then reports depends on whether anything else in the composition
supplies a repository. In most compositions it is a missing package:

```
Updating Subscription Management repositories.
subscription-manager is operating in container mode.
No match for argument: rhel-system-roles
Error: Unable to find a match: rhel-system-roles
```

If this fragment is the only one in the composition and the base ships no
repository files of its own, as `rhel-bootc` does not, the message is instead
`Error: There are no enabled repositories in "/etc/yum.repos.d", ...`.

Nothing in either output names the placeholder. Diagnosing a failed build
therefore starts from which half of the pair the manifest names, not from the
error text.

## Editing the placeholder files

Keep the explanatory text in the placeholder PEM files free of the colon
character. A colon anywhere above or inside a PEM block crashes the certificate
parser in `python3-subscription-manager-rhsm` 1.30.12 with SIGSEGV, and the build
then fails with a bare `exit status 139` that names nothing at all. Measured on
RHEL 10.2, aarch64.
