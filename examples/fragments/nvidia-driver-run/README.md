# nvidia-driver-run

Installs the NVIDIA driver from the vendor's `.run` self-extracting installer,
compiling the kernel modules for the kernel that the image being built ships,
not the kernel of the machine running the build.

## Who this is for

The users of a fragment like this are third-party vendors, or the users of a
third-party component, who need help maintaining that component's integration
into a single Containerfile. The pitch is not "NVIDIA on bootc". It is that you
should not have to maintain forty lines of installer-taming inside your own
Containerfile, re-derived and re-debugged in every project that needs the same
component. You consume it as a versioned, pinned artifact instead, and your
Containerfile says one line.

## Why this exists

The idiomatic way to get an NVIDIA driver into a bootc image is RPMs, from
NVIDIA's CUDA repo or from a prebuilt kmod package. Nobody in the bootc
ecosystem runs the `.run` at image build time.

This fragment is the deliberately hard case. It exists because a vendor
installer that wants a TTY, wants to write its own modprobe rules, wants to
rebuild your initramfs, and needs a compiler that must not survive into the
finished image is the most demanding thing the hooks contract can be asked to
hold. If the format can carry this, the format can carry a bad idea, which is a
more interesting claim than being able to carry a good one.

## Building the fragment image

The installer is roughly 350 MB and is never committed to this repository. Fetch
it first; the script verifies a recorded sha256 before the blob is used, and
extracts the `LICENSE` that has to travel with it.

```bash
cd examples/fragments/nvidia-driver-run
./fetch-run-installer.sh                 # defaults to this machine's architecture

podman build \
  --annotation com.github.marrusl.osfragment.name=nvidia-driver-run \
  --annotation com.github.marrusl.osfragment.version=610.57.04 \
  -f Containerfile.fragment \
  -t quay.io/your-username/nvidia-driver-run:610.57.04 .
```

The fragment image is architecture-specific, because the installer is. The
entrypoint is not: it derives the installer filename from `uname -m`, which in a
build container is the target image's architecture.

`hooks/*.run` and the two extracted `LICENSE` copies are listed in the repo's
`.gitignore`. Everything else in the fragment is committed.

## What the entrypoint does, and why it is one script

`hooks/entrypoint` is the only file the tool runs. It:

1. Derives `$KVER` from the image's own `kernel-core` package, and refuses to
   continue if the image somehow carries more than one kernel. `uname -r` would
   report the build host's kernel and is never correct here.
2. Runs the archive's own `--check`, so a truncated download fails early and
   legibly instead of halfway through a compile.
3. Installs the build toolchain, builds and installs the driver, runs `depmod`
   against `$KVER`, verifies the modules actually landed, then removes the
   toolchain and the package caches.

Step 3 is all one script on purpose. The tool bind-mounts `hooks/` for the
duration of a single `RUN`, and a bind mount is never committed to a layer, so
the 350 MB installer contributes zero bytes to the finished image. The toolchain
is installed and removed inside that same `RUN` for the same reason: had it been
declared as `packages.required` in `fragment.toml`, it would have been installed
during the earlier batched `dnf` step, and removing it later would write a
whiteout while the bytes stayed recoverable in that earlier layer forever.

The fragment owns nouveau blacklisting rather than letting the installer do it
(`--no-disable-nouveau`), so `tree/` carries the modprobe.d drop-in and the
`bootc` `kargs.d` file as plain, reviewable files. The tool copies them verbatim
and understands nothing about either format.

Nothing here registers the module for later rebuilds. DKMS is deliberately
absent: on a running bootc system `/usr` is read-only, so a boot-time DKMS
rebuild would try to write modules into a read-only filesystem and fail. The
module is fully baked at build time for exactly one kernel version.

## Base image compatibility

The fragment is kernel-agnostic by construction: `$KVER` comes from the base
image's own `kernel-core` at build time. The requirement is simply that the base
image's kernel NVR still has a matching `kernel-devel` in the repositories the
image itself configures, which in practice means a current image.

Measured against CentOS Stream on 2026-08-04: the configured repos carried five
`kernel-devel` builds for both Stream 9 and Stream 10, so the window is
comfortably wide rather than a single current NVR. Worth knowing, though, is
that the repos carry a subset of builds rather than every consecutive one, and
on that particular day the stock `centos-bootc:stream10` and `:stream9` images
each shipped a kernel whose exact NVR was absent while its immediate neighbours
were present. When that happens the build stops at `dnf install kernel-devel`
with a message saying so and listing the builds that are available. That is a
property of the base image being out of step with its own repos, not of this
fragment, and no out-of-band package source is used to paper over it.

The example manifest deliberately does not pin the base by digest, since "any
current base, no pin, the entrypoint adapts" is the point. Pin by digest if you
want byte-reproducible rebuilds.

The residual risk is orthogonal to repositories: a pinned driver version will
eventually meet a kernel too new to compile against, because out-of-tree modules
chase kernel API churn. That fails loudly at build time, and the fix is bumping
the fragment's driver version. That is the versioned-fragment model working as
intended rather than a defect.

## Maintenance is your rebuild cadence

In image mode, driver maintenance stops being a thing you do to machines and
becomes a property of how often you rebuild. Each rebuild pulls a newer base,
and the entrypoint recompiles the driver against whatever kernel it finds there,
so new images get new kernels and working modules without anyone intervening.
The only deliberate maintenance event left is occasionally bumping the
fragment's pinned driver version, which is a one-line change to a versioned
artifact. The consuming Containerfile never changes at all.

## License

The NVIDIA Driver License Agreement (v. February 25, 2025) permits distribution
of the software for use with OSI-licensed operating system kernels at section
1.1(d), provided the binaries are unmodified and the agreement reaches each
recipient. This fragment therefore ships the installer byte-for-byte as
downloaded, and ships NVIDIA's `LICENSE` at both hops: `hooks/LICENSE` travels
with the blob for recipients of the fragment, and
`tree/usr/share/licenses/nvidia-driver-run/LICENSE` lands in the built image for
recipients of the OS image. Both are extracted from the verified archive itself,
so they always match the binary they accompany.

`--accept-license` is not passed, because it is obsolete and ignored by current
`nvidia-installer`. Use of the driver implies acceptance of that `LICENSE`.

## Variant: fetch at build time instead of embedding

If you would rather not redistribute the installer, the fragment becomes a few
kilobytes and the only change is in the entrypoint: replace the `RUN_FILE`
assignment with a pinned `curl` plus a `sha256sum -c` check against a hardcoded
digest, into a path under `/var/tmp` that the same `RUN` removes. Everything
else, including the fact that no installer bytes reach the finished image, is
unchanged. The tradeoff is a build-time network dependency on
`download.nvidia.com` in exchange for no redistribution exposure.

## Out of scope

The modules this fragment builds are unsigned, exactly as any DKMS or akmods
build would be, so Secure Boot needs a signing key and MOK enrollment. Module
signing is orthogonal to the fragment format; `nvidia-installer` has
`--module-signing-secret-key` if you want to wire it up.
