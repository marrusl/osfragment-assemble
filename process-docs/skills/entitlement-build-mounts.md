# RHEL entitlement as a build mount

Proven end to end 2026-08-08 and 2026-08-09 against a live entitlement, on two
builders and three base images, including one registered RHEL host. What follows
is measured, not inferred, except where marked.

**Provenance of the test material, stated because it changes what was proven.**
The entitlement used throughout is a registered RHEL host's own, extracted from
that host. It was carried to an unsubscribed builder as a fragment and worked
there. The claim that establishes is that a credential can be decoupled from the
machine it belongs to and delivered as a pinned artifact. It is not a claim that
one machine's credential was used across a fleet of others.

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
  `redhat.repo` plus curl 77 means the CA is missing.
- **Three states of `redhat.repo`, three different causes.** Populated means the
  entitlement parsed and carried content sets, so any failure past that point is
  CA or network. Written but with zero sections means the certificate parsed and
  is not currently valid: that is the clock-skew signature below, where
  `notBefore` had not arrived. **Absent** means the certificate did not carry an
  entitlement at all, which is what a placeholder produces. Check with
  `ls /etc/yum.repos.d`, and note that absent looks identical to the base image's
  own shipped state, so it reads as "unchanged" rather than "broken".
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

The derivation rule also has a silent edge: an **empty directory beside a
populated one derives nothing** for the empty path, and the empty-mount notice
fires only when the whole `mount/` holds no files. A fragment shipping some
paths as bare directories (a partial exemplar, say) passes validation, quietly
mounts less than intended, and fails much later at dnf with an error that
points at credentials rather than at the missing mount.

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

## A colon in a placeholder PEM segfaults subscription-manager

Measured 2026-08-09 on `rhel-bootc` 10.2, aarch64,
`python3-subscription-manager-rhsm-1.30.12-1.el10`. A PEM file whose body
contains a colon crashes `rhsm.certificate.create_from_file` with SIGSEGV, at
`rhsm/certificate2.py:106`, `_certificate.load(path)`. Minimal reproducer, no
entitlement required:

```
-----BEGIN CERTIFICATE-----
a
b
c
d: e
-----END CERTIFICATE-----
```

Colon anywhere in the body: SIGSEGV. Colon absent: clean Python exception, exit
1. `openssl x509` on the same bytes exits 1 either way, so this is
subscription-manager's C extension, not OpenSSL.

The consequence for build mounts: a placeholder certificate written as prose
between PEM markers is one ordinary English sentence away from turning a
diagnosable build failure into

```
while running runtime: exit status 139
```

which names nothing at all. **Placeholder entitlement material should be a
syntactically valid throwaway self-signed certificate**, with any explanation in
a colon-free preamble above the `BEGIN` line, where PEM readers ignore it.

## What a placeholder actually fails with, and why the message varies

Measured end to end with tool-generated Containerfiles and a digest-pinned
example fragment. A valid but non-entitlement certificate at the live paths
**never generates a repository file at all**. What dnf then says depends on
whether anything else in the build supplies one:

| Composition | Base | Message |
|---|---|---|
| entitlement fragment alone | `rhel-bootc` | `Error: There are no enabled repositories in "/etc/yum.repos.d", ...` |
| entitlement fragment + any repo-providing fragment | `rhel-bootc` | `No match for argument: <pkg>` / `Error: Unable to find a match: <pkg>` |
| placeholder shadowing the host's injection | `ubi10/ubi` | `No match for argument: <pkg>` |

Package-not-found is the general case. The louder message needs two conditions
at once: the composition contributes no repo file, and the base ships none.
`rhel-bootc`'s empty `/etc/yum.repos.d/` is what supplies the second, so this is
not evidence about entitlement at all. Do not quote the louder message as *the*
placeholder failure signature; the durable statement is **no repository file is
generated**, checkable with `ls /etc/yum.repos.d`.

The certificate itself parses: rhsm reads it as an `IdentityCertificate`, finds
no entitlement, and generates nothing. Because repo generation never runs, the CA
is never consulted, so a placeholder CA is never the reported cause. The CA can
only be the reported problem when the entitlement is good.

## Measured on a registered host, not inferred

The earlier version of this file reasoned about subscribed build hosts from the
`mounts.conf` symlink chain. Measured directly on RHEL 10.2,
`containers-common-5.8-2.el10`, registered:

During an ordinary build with no explicit mounts, podman injects

```
/run/secrets/etc-pki-entitlement/<serial>.pem      <- 2 entries, NON-EMPTY
/run/secrets/etc-pki-entitlement/<serial>-key.pem
/run/secrets/redhat.repo
/run/secrets/rhsm/{rhsm.conf,ca/{redhat-uep,redhat-entitlement-authority}.pem}
/run/secrets/rhsm/{facts/insights-client.facts,syspurpose/{syspurpose,valid_fields}.json}
```

Both container-mode probes pass and `rhsm.config.in_container()` returns `True`.
The non-empty clause is satisfied by the host, unasked, on every build.

**The decoy result is therefore real, and here it is as a matched pair.** One
fragment, one placeholder certificate and key, two mount targets, everything else
identical:

| Mount target | Build |
|---|---|
| `/etc/pki/entitlement` (the real path) | **succeeds** — the mount is silently ignored, the host's injection wins |
| `/run/secrets/etc-pki-entitlement` | **fails** — the mount is consulted and shadows the host |

`ls` inside both RUNs shows the placeholder files present at the target, so the
mount happened either way. Only its effect differed. This is why `/run/secrets/`
is the target: it is the only one the host cannot override.

## The built image carries the entitlement serial, and always has

The mount is not committed, but the `redhat.repo` the dnf plugin generates at
build time is: 98130 bytes, 182 sections, 182 lines of
`sslclientcert = /etc/pki/entitlement-host/<serial>.pem`. No key material
(`/etc/pki/entitlement` empty, `/run/secrets` absent), but the serial identifies
the subscription and it persists in a layer. Strip or regenerate that file before
publishing anything built this way.

**This is not the fragment's doing.** An entitled build on a registered host with
podman's automatic passthrough and no fragment anywhere commits the identical
file, same byte size, same 182 serial-naming lines. Measured on both paths. A
fragment that carried the credential is no worse than the status quo here.

**Verify committed content with `podman image mount`, never `podman run`.** On a
subscribed host `podman run` re-applies the `/run/secrets` injection, so a
listing of that path shows the host's files and tells you nothing about the
image. `podman unshare bash -c 'm=$(podman image mount <img>); ls -A "$m/run/secrets"'`
reads the layers.

**Deleting it from a hook does not work.** Hooks run in their own `RUN`, after the
package `RUN` that wrote the file, so `rm` writes a whiteout and the bytes stay
recoverable in the earlier layer. Only a removal inside the RUN that created the
file keeps the bytes out, and that RUN is generator-emitted. See
`containerfile-layer-semantics.md`.

## A local digest is not a registry digest, and the cause is recompression

`skopeo copy` is byte-faithful: registry to registry, the destination digest
equals the source manifest's digest exactly. Verified against
`rhel10/rhel-bootc:latest`'s arm64 child and again by copying `ubi10/ubi` into a
`dir:` destination and hashing the result.

`podman push` is not, and it is not a format question. Pushing the same image
from local storage published a different manifest digest, and comparing the two
manifests directly: same byte length, same media type, same annotations,
**identical config digest**, and **65 of 66 layer digests different**, each pushed
layer slightly larger. An identical config digest means identical diffIDs, so the
content is the same and only the compressed blobs differ. Local storage does not
keep compressed layer bytes, so every push re-gzips and mints new blob digests.

`--format` does not fix it. Default and `--format oci` produce one digest,
`--format v2s2` another, and neither reproduces the upstream one. And `skopeo copy`
*from* the local registry reproduces podman's digest, because skopeo faithfully
copies whatever its source holds. The transports agree; the rewrite happens on the
way out of local storage.

**The rule, measured on a throwaway image:**

```
built here, never pushed      local    sha256:d81a6237...
after podman push             registry sha256:a091884a...   <- different
podman rmi, then pull back    local    sha256:a091884a...   <- local now matches
re-push that pulled copy      registry sha256:a091884a...   <- stable
```

`skopeo inspect containers-storage:` agrees with `podman image inspect` at every
step. So a local image has one digest whose meaning depends on provenance: for an
image **built** here it is not what a registry will assign; for an image **pulled**
here it is. Take a pin from the registry, never from `podman image inspect`.

This is the gating constraint on any future `containers-storage:` fragment source:
a digest read from local storage for a freshly built fragment is one nobody else
can resolve, and the mandatory pin rule would be satisfied by a wrong string.

## Local registry that survives a reboot

Rootless Quadlet, verified across an actual reboot:

```ini
# ~/.config/containers/systemd/local-registry.container
[Container]
ContainerName=local-registry
Image=docker.io/library/registry:2
PublishPort=5000:5000
Volume=local-registry-data:/var/lib/registry

[Service]
Restart=always

[Install]
WantedBy=default.target
```

`sudo loginctl enable-linger <user>` is the part that is easy to miss: without
it the user manager does not start at boot and the unit never runs. Then
`systemctl --user daemon-reload && systemctl --user start local-registry`.

`registry:2` has deletes disabled by default, so removing a pushed image needs
`REGISTRY_STORAGE_DELETE_ENABLED=true` plus a garbage collection pass. For a
test registry holding credential material it is faster and more certain to stop
the unit, `podman volume rm -f <volume>`, start it again, and re-push the
keepers. Re-pushed fragment images keep their digests, so pinned manifests
survive that.

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

**Never verify with `find / -xdev`.** On a stock CentOS Stream 9 install `/home`
is a separate logical volume, so `-xdev` stops at the mount point and reports
nothing while the certificate and key are sitting in `~/fragments` and in the
rootless image store. Measured: `sudo find / -xdev -name '<serial>*'` returned
zero results on a host holding four copies.

For a keeper, three places hold the material and they need separate treatment:
the fragment source tree, the rootless image store
(`.../storage/overlay/<layer>/diff/fragment/mount/...`, uncompressed), and the
registry's volume (compressed, filenames are digests, so no name or content
search finds it). `podman rmi` reaches only the second.

Run a throwaway test registry with **no volume** so its blobs die with the
container. A registry meant to survive reboots needs a volume, and that volume
is then a durable copy of every credential ever pushed to it.

## Transferring a fragment tree off a Mac

`tar czf -` on macOS writes AppleDouble `._*` companion files into the archive.
Unpacked on the builder, that puts a regular file directly under `mount/`, which
is a generation error, and `._<name>` siblings beside every real file. Set
`COPYFILE_DISABLE=1` for the tar, or build the tree on the builder.

## Credential mounts must ride every hook RUN, not just the package RUN

Measured 2026-08-09 on a full RHEL 10 arm64 build: the entitlement fragment
worked on the batched `dnf install` but a fragment hook that shelled out to
`dnf install unzip` (base-OS package) failed with `could not load PEM client
certificate from /etc/pki/entitlement-host/<serial>.pem ... No such file or
directory`. That path is a base-shipped symlink into
`/run/secrets/etc-pki-entitlement`; the committed `redhat.repo` names the
`-host` path, and on an unmounted step the symlink target does not exist, so it
dangles. Same mechanism as the rest of this file, on a different RUN.

The generator cannot introspect a hook, and hooks assume the base repos are
reachable, so the fix is to mount the credential build-mounts on **every
dnf-capable RUN** unconditionally: the package step and every hook step. The
correctness requirements a future edit must not regress:

- **`mount_flags` is built unconditionally, outside the `if !all_packages.is_empty()`
  block.** A composition with mount + hook fragments but zero packages emits no
  package RUN, so the hook RUNs are the only place the creds attach. Moving
  `mount_flags` construction inside the package block silently strips creds from
  hooks in that case. This is the highest-value regression; it has a dedicated
  test (`credential_mounts_ride_the_hook_run_with_no_package_step`).
- **Reuse the `mount_flags` vector verbatim on each hook RUN; never route it
  through `copy_from_source`.** Build-mount references are always inline
  (`from=<pinned ref>` in registry mode, `from=`-less `context_source` in
  self-contained mode), and a pure-mount fragment has no `frag-<name>` named
  stage to reference. The hook's own `/frag-hook` mount rides the `RUN` line;
  the credential mounts follow it as continuation lines. Both sites emit through
  the shared `write_mounted_run` helper so their continuation formatting cannot
  drift.
- **`unattached_mount_notice` fires only when there is no dnf-capable step at
  all** (no packages AND no hooks). Before the fix it keyed on packages alone;
  after it, a mount + hooks + no-packages composition does emit the mounts (on
  hook steps), so the old "no mount is emitted" notice would be a false positive.
- **Security posture widens by one step class:** trusted-but-arbitrary hook code
  now reads the credential at `/run/secrets` during its RUN. Still ephemeral and
  uncommitted by the generator, but a hook could copy the material into a layer,
  so pair an entitlement fragment only with hook fragments you trust. `ro` also
  blocks a hook persisting writes back through the mount.
- **Subscribed-host guard is docs-only.** On a subscribed RHEL host the host
  auto-injects `/run/secrets` on every step, so the fragment is redundant; omit
  it there. No opt-out flag: the tool emits a static Containerfile and cannot
  know the build host, so the warning is an in-file Containerfile comment (it
  travels to the build host / CI).
