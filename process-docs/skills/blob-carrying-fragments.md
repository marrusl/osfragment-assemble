# Fragments that carry a large vendor blob

Covers fragments whose `hooks/` holds a big binary payload (the NVIDIA `.run`
example is the first), and how to test one end to end locally without pushing
anything to a public registry.

## The blob is fetched, never committed

A ~300 MB vendor installer must not enter a public repo's git history. The
pattern the `nvidia-driver-run` example established:

- A `fetch-<something>.sh` script inside the fragment directory downloads the
  pinned payload into `hooks/` and verifies a **recorded sha256** before the
  blob is used. The digest is captured by downloading the file once and
  recording what came back; there is no upstream checksum file to consult for
  the NVIDIA installers.
- An architecture with no recorded digest is **refused**, not installed
  unverified. The payload is architecture-specific, so the fragment image is
  too, even though the entrypoint is not.
- The blob and anything derived from it are listed in the repo `.gitignore`.
  For the NVIDIA fragment that includes both `LICENSE` copies, because they are
  extracted from the verified archive rather than committed separately, which
  keeps the shipped license matching the shipped binary automatically.

`sh <installer>.run --extract-only` works on macOS, so the license extraction
does not need a Linux container.

## Local end-to-end testing

Use the local registry at `localhost:5050` plus `--self-contained`, which
materializes the fragment into a build context so `podman build` needs no
registry except the base.

**The mirror trick from `registry-verification.md` does not work here.**
Redirecting `quay.io/marrusl2/fragments` to the local registry lets the *pull*
succeed but the run still dies in the digest lookup:

```
Error: skopeo digest lookup failed for quay.io/marrusl2/fragments/...:
  Error determining repository tags: fetching tags list: name unknown:
  repository not found
```

That lookup resolves tags against the primary location and ignores the mirror,
so a fragment that exists **only** locally must be referenced by its
`localhost:5050/...` name in the manifest under test. Mirroring still works for
fragments that exist at both locations, which is the case the other skill
describes.

Materializing a 305 MB fragment into a self-contained context took about 7
seconds and produced a `.tar.gz` no smaller than the directory, because an
already-compressed installer does not gzip further.

## The podman machine fills up fast

A bootc base is ~1.6 GB and every experimental build keeps its own copy of the
layers. A default 30 GiB `podman machine` disk hit `no space left on device`
mid-`COMMIT` with 341 images present. `podman image prune -f` reclaimed almost
nothing because the space was in **named** leftovers from earlier builds, not
dangling layers; removing those tags and pruning again freed 15 GB. Check
`podman machine ssh "df -h /var"` before starting, not after a failure.

## Base image kernel and repo coherence

Any fragment that compiles a kernel module needs `kernel-devel` matching the
base image's kernel **exactly**. Measured on 2026-08-04, both
`centos-bootc:stream10` and `centos-bootc:stream9` shipped a kernel whose exact
NVR their own configured repos did not carry, while neighbouring builds were
present:

| base | image `kernel-core` | `kernel-devel` in repos |
|---|---|---|
| stream10 | `6.12.0-253.el10` | 246, 248, 250, 251, **254** |
| stream9 | `5.14.0-730.el9` | 721, 722, 725, 729, **731** |

So the repos keep five builds, which is a comfortable depth, but they keep a
*subset* rather than every consecutive build, and the rolling tag's own kernel
can fall in a gap. Pinning the base by digest does not help; the current digest
is the one that fails.

To get a coherent base for a local experiment, sync the kernel to the
repo-current NVR the way any derived image doing a routine `dnf update` would:

```dockerfile
RUN OLD_KVER="$(rpm -q --qf '%{VERSION}-%{RELEASE}.%{ARCH}' kernel-core)" && \
    dnf -y update kernel kernel-core kernel-modules kernel-modules-core && \
    dnf -y remove "kernel-core-${OLD_KVER}" "kernel-modules-${OLD_KVER}" \
                  "kernel-modules-core-${OLD_KVER}"
```

The explicit removal matters because the kernel is an install-only package, so
the update leaves two kernels behind. `--setopt=installonly_limit=1` is not a
way around it: dnf5 rejects the value outright with
`value 1 is not allowed`.

## Driver version and kernel version are a matched pair

CentOS Stream 10's `6.12` kernel carries heavy DRM and PCI backports from much
newer upstream kernels, so "the driver supports 6.12" is not sufficient
reasoning. NVIDIA 580.95.05 failed to build against `6.12.0-254.el10` with
`implicit declaration of function 'DRM_ERROR'`, `'struct __drm_crtcs_state' has
no member named 'state'`, `too few arguments to function 'pci_resize_resource'`,
and `'const struct dma_map_ops' has no member named 'map_resource'` — all
symptoms of the kernel being *newer* than the driver's feature detection
expects, not older. Reach for a newer driver branch when those appear.

## Any entrypoint that runs dnf leaves rhsm residue, and it is easy to misread

A single `dnf` invocation inside a hook creates all of these on
`centos-bootc:stream10`, none of which exist in the stock base (measured
2026-08-04):

| path | left by | costs a lint warning |
|---|---|---|
| `/run/rhsm` | subscription-manager dnf plugin | yes, `nonempty-run-tmp` |
| `/var/log/rhsm/rhsm.log` | subscription-manager dnf plugin | yes, `var-log` + `var-tmpfiles` |
| `/var/lib/rhsm/` | subscription-manager dnf plugin | yes, `var-tmpfiles` |
| `/var/cache/ldconfig/aux-cache` | ldconfig | yes, `var-tmpfiles` |
| `/etc/yum.repos.d/redhat.repo` | subscription-manager dnf plugin | no |
| `/usr/share/rpm/rpmdb.sqlite-{shm,wal}`, `.rpm.lock` | rpm | no |

The first four must be removed in the same `RUN`, on top of the dnf caches and
logs, or the build ships three extra lint warnings.

**Only the last dnf caller's cleanup is visible in the finished image.** These
paths are recreated by every `dnf` invocation and removed by whichever step
cleans up after itself, so an earlier step's diligence proves nothing about what
ships. On the demo composition the generator's package step and the `awscli-zip`
hook both cleaned up correctly, and the image still carried all four warnings
because the `nvidia-driver-run` hook — the last step to run `dnf` — did not
(measured 2026-08-05). **Attribute this residue by walking the intermediate
layers** (`podman run <layer-id> ls /run/rhsm`), never by reading the hooks:
reading produced the wrong answer twice.

**The trap:** the NVIDIA experiment recorded the rhsm paths as base artifacts.
They are not. That run tested against a scaffold base which had itself run `dnf`
to update the kernel, so the residue was already present before the fragment
ran and looked like it belonged to the base. **Always diff against the *stock*
base, not against whatever scaffold the experiment needed.** Stock
`centos-bootc:stream10` lints with exactly one warning
(`var/roothome/buildinfo/content-sets.json`); that is the number a fragment has
to match to be clean.

The last two rows are residue that `bootc container lint` does not check for.
They are left in place: the rpmdb sidecars are 0-byte and 32 KB with the rpmdb
fully queryable afterwards, and `redhat.repo` is a 376-byte auto-generated stub
with no repo definitions. Removing `redhat.repo` unconditionally would be wrong
on a RHEL base, where subscription-manager legitimately owns it.

## Remove only the build tooling you installed

A fragment that installs a tool in its entrypoint must not unconditionally
remove it, or it strips that tool from every base that ships it deliberately.
Check first and remember:

```bash
INSTALLED_UNZIP=0
if ! command -v unzip >/dev/null 2>&1; then
    dnf -y install unzip
    INSTALLED_UNZIP=1
fi
# ... later, in the same RUN ...
if [[ "$INSTALLED_UNZIP" -eq 1 ]]; then
    dnf -y remove unzip
fi
```

For reference, none of `centos-bootc:stream10`, `centos-bootc:stream9`, or
`fedora-bootc:42` ships `unzip` (measured 2026-08-04).

## `python3 -m zipfile` is not a substitute for `unzip`

Reaching for the base's Python to avoid installing an extraction tool looks
attractive and silently breaks executables. A zip records permissions, and
`python3 -m zipfile -e` discards them: the AWS CLI bundle stores `aws/install`
and `aws/dist/aws` as `-rwxr-xr-x` and Python extracts both `-rw-r--r--`
(verified 2026-08-04). Recovering means `chmod`-ing whichever paths the payload
happens to need, which is worse to maintain than installing a 186 KB package
and removing it in the same layer.

## A fragment may carry `hooks/` with no `tree/`

`awscli-zip` is the first example shaped that way and the loader handles it —
`inspect` reports the fragment with only a `hooks/` listing. Omit the
`COPY tree/` line from `Containerfile.fragment` entirely; do not ship an empty
`tree/` to keep the shape uniform.
