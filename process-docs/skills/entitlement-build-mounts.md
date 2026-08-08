# RHEL entitlement as a build mount

Proven end to end 2026-08-08 against a live entitlement, on two builders and
three base images. What follows is measured, not inferred, except where marked.

## The mount target is `/run/secrets/`, and the reason is control flow

Mount entitlement at `/run/secrets/etc-pki-entitlement` and `/run/secrets/rhsm`,
not at `/etc/pki/entitlement` and `/etc/rhsm`. This is not a naming convention.

`rhsm/config.py` decides it is "in a container" by testing two paths, which are
symlinks the base image ships:

```python
HOST_CONFIG_DIR   = "/etc/rhsm-host/"          # -> /run/secrets/rhsm
HOST_ENT_CERT_DIR = "/etc/pki/entitlement-host" # -> /run/secrets/etc-pki-entitlement

def in_container() -> bool:
    if os.path.isdir(HOST_CONFIG_DIR) and (
        os.path.isdir(HOST_ENT_CERT_DIR) and any(os.walk(HOST_ENT_CERT_DIR))
    ):
        return True
```

When that returns True, `RhsmHostConfigParser` rewrites `ca_cert_dir` and
`repo_ca_cert` from `/etc/rhsm/` to `/etc/rhsm-host/`, and redirects
`entitlementCertDir` to `/etc/pki/entitlement-host`. **The real paths stop being
consulted.**

The consequence that decides the design: on a build host that is itself a
registered RHEL machine, podman auto-populates
`/run/secrets/etc-pki-entitlement` from the host (see below), `in_container()`
flips to True, and a fragment mounting the real paths is **silently ignored**.
Verified: a build with a good entitlement at `/etc/pki/entitlement` and a
non-empty decoy at `/run/secrets/etc-pki-entitlement` fails with
`No match for argument: <pkg>`.

The `/run/secrets` target also masks nothing the base needs, whereas mounting
`/etc/rhsm` would mask `facts/` and `syspurpose/`.

## Minimum viable set: exactly four files

```
mount/run/secrets/etc-pki-entitlement/<serial>.pem
mount/run/secrets/etc-pki-entitlement/<serial>-key.pem
mount/run/secrets/rhsm/rhsm.conf
mount/run/secrets/rhsm/ca/redhat-uep.pem
```

**Do not ship `redhat-entitlement-authority.pem`.** It sits next to
`redhat-uep.pem` in the host dump and is unused by this path.

`redhat.repo` is **not** required either. The `subscription-manager` dnf plugin
regenerates it inside the image from the mounted entitlement, with the correct
cert serial in `sslclientcert`. Shipping it also triggers the mount collapse
described below.

Non-obvious members of the set:

- **`rhsm.conf` is required.** Without it the parser falls back to defaults and
  `repo_ca_cert` interpolation never resolves. The failure prints the raw
  `%(ca_cert_dir)sredhat-uep.pem` in a curl error, which does not look like a
  missing-config problem.
- **`redhat-uep.pem` is required even though the base ships it** at
  `/etc/rhsm/ca/`. Once `in_container()` is True that path has been rewritten to
  `/etc/rhsm-host/ca/`, so the base's copy is unreachable.

### Which CA, precisely

Isolated with four fragment variants differing only in `ca/`, on `rhel-bootc`,
on a host whose `mounts.conf` targets all dangle:

| `ca/` contents | Result |
|---|---|
| uep + authority | pass |
| **uep only** | **pass** |
| authority only | fail, `Curl error (77): Problem with the SSL CA cert` |
| empty | fail, same error |

`redhat-entitlement-authority.pem` is referenced by **no code anywhere** in the
image (recursive grep over both `site-packages` trees and `/etc/rhsm` returns
zero hits). The file that matters is named by a hardcoded default,
`rhsm/config.py`'s `"repo_ca_cert": "%(ca_cert_dir)sredhat-uep.pem"`, consumed at
`repolib.py:502` when writing each repo entry.

The one path that could have used it never runs in a container: `ca_cert_dir` is
a *directory* trust store for the Candlepin connection (`rhsm/connection.py:274`),
and both the dnf plugin and `repolib.py:319` guard that connection with
`not config.in_container()`. That short-circuits **regardless of
`full_refresh_on_yum`**, so no setting brings the file back into play.

**The durable rule** (correct for Satellite and mirrored setups too, where
`repo_ca_cert` points at that infrastructure's own CA instead):

> carry the entitlement cert, its key, `rhsm.conf`, and whatever file
> `repo_ca_cert` in that `rhsm.conf` resolves to.

### Two diagnostic facts from the same run

- **`redhat.repo` generation does not depend on the CA.** All four variants
  produced the full 182 sections, including the two that failed. A populated
  `redhat.repo` plus curl 77 means the CA is missing; an *empty* `redhat.repo`
  means the entitlement cert is the problem instead.
- **`sslcacert` in the generated file is not evidence the CA exists.** Every
  variant emitted `sslcacert = /etc/rhsm-host/ca/redhat-uep.pem`, including the
  ones with no such file mounted. The value is copied from `repo_ca_cert`
  unconditionally.

## The build host auto-injects `/run/secrets`, and it will contaminate a test

`containers-common` ships `/usr/share/containers/mounts.conf`:

```
/usr/share/rhel/secrets:/run/secrets
```

with `/usr/share/rhel/secrets/{etc-pki-entitlement,rhsm,redhat.repo}` symlinked
to the host's `/etc/pki/entitlement`, `/etc/rhsm`, and
`/etc/yum.repos.d/redhat.repo`. **Every container and every build step gets
this**, unasked.

Present on both the Fedora CoreOS podman machine (`containers-common-0.67.0-1.fc44`)
and CentOS Stream 9 (`containers-common-5.8-1.el9`). It is RPM-family packaging,
not a podman-machine or Podman Desktop artifact.

What it means in practice:

- On a podman machine VM, `/run/secrets/rhsm` arrives **already populated with
  both CA certificates**, because `subscription-manager-rhsm-certificates` is
  installed in the VM. A subtraction test that leaves that path uncovered will
  conclude the CA certs are unnecessary. They are not.
- On CentOS Stream 9 with no subscription, all three symlinks dangle, so the
  host contributes nothing. That is the clean environment.

**Always run two negative controls before trusting a subtraction result:** a
build with nothing mounted (must fail), and a build with an empty directory
mounted over `/run/secrets` (must also fail). Then confirm each passing result
mounted over the path the host would otherwise supply.

## `mount/run/secrets/redhat.repo` collapses three mounts into one

`derive_mount_points` takes every directory that directly contains a file, then
prunes any nested inside another. Adding `mount/run/secrets/redhat.repo` makes
`run/secrets` itself a mount point, so both `etc-pki-entitlement` and `rhsm` are
pruned and the fragment emits a **single** mount of `/run/secrets`.

That variant works, and has a real advantage: it masks the host's `mounts.conf`
injection completely, making the build host-independent. It costs a large stale
file in the fragment. Choose deliberately; do not arrive there by accident.

## What the RHEL bootc base actually ships

`registry.redhat.io/rhel10/rhel-bootc:latest` (10.2), measured:

- `/etc/yum.repos.d/` is **completely empty**. No `redhat.repo`, static or
  otherwise. It could not be static: a shipped file cannot know a cert serial.
- `/etc/rhsm-host` and `/etc/pki/entitlement-host` symlinks present. **Owned by
  no RPM** on any image checked; they come from the container image build, so
  they are a base-image property, not a package property.
- `/etc/rhsm/ca/{redhat-uep,redhat-entitlement-authority}.pem` and
  `/etc/rhsm/rhsm.conf` present.
- `/etc/dnf/plugins/subscription-manager.conf` has `enabled=1`.
- `/etc/pki/product-default/419.pem` present.

`ubi10/ubi` matches on every point. `quay.io/centos-bootc/centos-bootc:stream10`
matches **except** that it has no `/etc/pki/product*` at all.

## A CentOS base needs a product cert; a CentOS builder does not

Same fragment, same mounts, on `centos-bootc:stream10`: `in_container()` returns
True, the plugin logs container mode, and `redhat.repo` is generated, but with
**zero enabled RHEL repositories**. `/etc/pki/product-default/419.pem` is what
maps entitlement content sets onto enabled repos, and CentOS has no product cert.

This is a property of the **base image**, not the builder. An entitled
`rhel-bootc` build runs fine on an unsubscribed CentOS Stream 9 *host*.

## Check the builder's clock before believing any certificate error

A CentOS Stream 9 VM 78 days behind produced three symptoms, none of which
mentions the clock:

1. `podman pull` from docker.io: `x509: certificate has expired or is not yet valid`
2. The entitled build reached container mode, then wrote a 376-byte
   `redhat.repo` with **zero sections**, because the entitlement cert's
   `notBefore` had not arrived yet
3. `openssl x509 -checkend 0` reported the cert **valid**, because `checkend`
   tests only expiry and never `notBefore`

`chronyd` was running with healthy sources and still would not correct it: the
offset exceeded its step threshold, so it slewed forever. Fix:

```bash
sudo timedatectl set-ntp true && sudo systemctl restart chronyd && sudo chronyc makestep
sudo hwclock --systohc     # else the RTC drags it back on reboot
```

Verify with `timedatectl status` and `chronyc tracking` before drawing any
conclusion about credentials.

## Local registry on a Linux builder is simpler than on the Mac

On a Linux host, registry and builder share a localhost, so:

```bash
podman run -d --name local-registry -p 5000:5000 docker.io/library/registry:2
mkdir -p ~/.config/containers/registries.conf.d
printf '[[registry]]\nlocation = "localhost:5000"\ninsecure = true\n' \
  > ~/.config/containers/registries.conf.d/999-localhost-5000.conf
podman push --tls-verify=false localhost:5000/fragments/<name>:<tag>
```

Rootless, no sudo, no restart. Contrast with the podman machine path in
`registry-verification.md`, which needs a VM drop-in via `podman machine ssh`
**and a `podman machine stop/start`**, because the long-running API service
caches `registries.conf` at startup. Skipping that restart produces a build
error that names no TLS problem at all:

```
Trying to pull localhost:5050/fragments/...@sha256:...
Error: ... no stage or image found with that name
```

which reads like a Containerfile typo and is not.

Also: `containers-storage:` on macOS refers to the **Mac's** storage, not the
VM's, so `skopeo copy containers-storage:... docker://...` cannot see an image
podman just built. Use `podman push`.

## Digest-pinned mounts cannot be shadowed by stale local images

`RUN --mount=from=<ref>` consults local storage first and pulls from the
registry on a miss (verified: with the image removed, the build printed
`Trying to pull ...` and proceeded).

A **digest** reference is content-addressed, so a local hit on `@sha256:X` is by
definition the same bytes the registry would serve. The stale-image trap
recorded in `registry-verification.md` was a *tag* reference. This is a second
reason for the existing rule that mount-carrying fragments must be digest-pinned,
independent of reproducibility.

## Build the binary before trusting its output

A stale `target/release/osfragment-assemble` fails **silently and wrongly**
rather than erroring. A binary two days behind `src/mount.rs`:

- printed no `mount/` section from `inspect`, for local dirs and registry images
- generated a Containerfile with **no mount flags at all**, just a bare
  `RUN dnf install`
- rejected a digest-pinned manifest entry with a doubled digest,
  `...@sha256:X@sha256:X`, reported as `skopeo copy failed`

All three vanished after `cargo build --release`. The doubled digest is not a
`pin_to_digest` bug; the current implementation strips an existing digest
correctly. `cargo` may not be on PATH; it lives under
`~/.rustup/toolchains/<toolchain>/bin/`.

## Credential hygiene

Entitlement material is a private key. It must never enter the working tree, be
staged, or be pushed to any remote, including quay.

When cleaning up a host afterwards, `podman rmi` by tag is **not sufficient**, and
neither is `podman image prune -f`. An image whose tag was removed keeps its
repository name, shows as `repo:<none>`, is therefore not "dangling", survives
the prune, and still holds the key. Remove those by **image ID**.

Verify by filename search for the cert serial rather than by reading
`podman images`:

```bash
find "$HOME" /tmp -name '<serial>*'
find "$HOME/.local/share/containers/storage" -path '*fragment/mount*'
```

Run the test registry with **no volume** so its blobs die with the container.
