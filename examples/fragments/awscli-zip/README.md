# awscli-zip

Installs AWS CLI v2 from the vendor's self-contained zip, into `/usr` rather
than the `/usr/local` the installer picks on its own.

## Who this is for

The users of a fragment like this are third-party vendors, or the users of a
third-party component, who need help maintaining that component's integration
into a single Containerfile. The pitch is not "the AWS CLI on bootc". It is that
you should not have to rediscover which arguments a vendor installer needs in
order to be correct for image mode, re-derived and re-debugged in every project
that installs the same component. You consume it as a versioned, pinned artifact
instead, and your Containerfile says one line.

## Why this exists

The idiomatic way to get the AWS CLI into a CentOS Stream 10 image is
`dnf install awscli2`. It is not a niche third-party package and it is not even
an EPEL package: `awscli2` is in **AppStream** (`2.33.0-1.el10`, checked
2026-08-04). As a fragment that would be four lines of `fragment.toml` with a
`packages.required` entry and no hook at all.

This fragment installs the zip anyway, because the zip is what AWS's own
documentation tells you to use and it is what a vendor ships when there is no
distro package. It is the deliberately hard case: a self-contained bundle with a
shell installer that has opinions about where your software lives, and whose
defaults are wrong for an image-mode system in a way that does not announce
itself.

## The one line that matters

`./aws/install` takes no required arguments. Run it that way and it does this,
measured on `quay.io/centos-bootc/centos-bootc:stream10` on 2026-08-04:

```
$ ./aws/install
You can now run: /usr/local/bin/aws --version

/usr/local/aws-cli/v2/2.36.16/          259 MB of vendor payload
/usr/local/aws-cli/v2/current  ->       /usr/local/aws-cli/v2/2.36.16
/usr/local/bin/aws             ->       /usr/local/aws-cli/v2/current/bin/aws
/usr/local/bin/aws_completer   ->       /usr/local/aws-cli/v2/current/bin/aws_completer
```

It works. `aws --version` runs. Nothing fails, which is the problem.

This fragment passes two arguments instead:

```sh
./aws/install --install-dir /usr/lib/aws-cli --bin-dir /usr/bin --update
```

That is the entire substance of the fragment. Everything else in
`hooks/entrypoint` is scaffolding around those two paths.

## Why `/usr/local` is the wrong answer here

`/usr/local` is the one path bootc has a documented opinion about. From bootc's
`docs/src/filesystem.md` (read 2026-08-04):

> The OSTree upstream recommendation suggests making `/usr/local` a symbolic
> link to `/var/usrlocal`. But because the emphasis of a bootc-oriented system is
> on users deriving custom container images as the default entrypoint, it is
> recommended here that base images configure `/usr/local` be a regular
> directory (i.e. the default).

So there are two possibilities, and which one you get is a property of the
base image, not of your build. Both are bad places for 259 MB of vendor payload,
for different reasons:

**If `/usr/local` is a regular directory** (which is what
`centos-bootc:stream10`, `centos-bootc:stream9`, and `fedora-bootc:42` all ship
today, all three checked 2026-08-04), the install lands inside `/usr`, and `/usr`
is mounted read-only on a running bootc system. The CLI works and updates with
the image, so nothing looks wrong. What you have actually done is take the path
the system reserves for the local administrator and fill it with image-owned,
read-only content. The uninstall instructions in AWS's own bundle
(`sudo rm -rf /usr/local/aws-cli`) now fail against a read-only filesystem, and
an admin who legitimately wants to put something in `/usr/local` is now
negotiating with your image for it.

**If `/usr/local` is the ostree symlink to `/var/usrlocal`** (which bootc's docs
describe as a choice for "final" images not intended to be derived from), the
payload lands in `/var` instead, and bootc is explicit about what that means:
content shipped in `/var` "is unpacked *only from the initial image* -
subsequent changes to `/var` in a container image are not automatically
applied." The CLI would be installed once, when the machine was first
provisioned, and then silently stay at that version forever while every other
part of the image moved on. Bumping the fragment's version would change nothing
on any already-provisioned host.

`/usr/lib/aws-cli` has neither problem. It is image-owned, it is read-only for
the same reason the rest of the OS is, it is replaced wholesale on every image
update, and it does not squat on the administrator's path. `/usr/bin` is where
the symlinks belong for the same reason: it is where `dnf install awscli2` would
have put `aws` too.

## Building the fragment image

The zip is roughly 70 MB and is never committed to this repository. Fetch it
first; the script verifies a recorded sha256 before the blob is used.

```bash
cd examples/fragments/awscli-zip
./fetch-awscli-zip.sh                    # defaults to this machine's architecture

podman build \
  --annotation com.github.marrusl.osfragment.name=awscli-zip \
  --annotation com.github.marrusl.osfragment.version=2.36.16 \
  -f Containerfile.fragment \
  -t quay.io/your-username/awscli-zip:2.36.16 .
```

The fragment image is architecture-specific, because the zip is. The entrypoint
is not: it derives the zip filename from `uname -m`, which in a build container
is the target image's architecture. Digests for both published architectures are
recorded in `fetch-awscli-zip.sh`; an architecture with no recorded digest is
refused rather than installed unverified.

`hooks/*.zip` is listed in the repo's `.gitignore`. Everything else in the
fragment is committed.

## What the entrypoint does

This is the minimal form a hostile-payload fragment can have: `hooks/entrypoint`
plus the vendor blob as hook material. There is no `tree/`, no
`packages.required`, and no repo definition: the fragment is a script and a
binary, and the two arguments are the whole point.

`hooks/entrypoint` is the only file the tool runs. It:

1. Refuses to continue if the pinned zip is not present as hook material.
2. Installs `unzip` **only if the base does not already provide it**, and
   remembers whether it did. None of the three bootc bases checked above ship
   `unzip`, so in practice it installs it; unconditionally removing it later
   would strip a tool that some other base deliberately ships.
3. Extracts to a scratch directory under `/var/tmp` and runs `./aws/install`
   with the two arguments above.
4. Places the bundle's `THIRD_PARTY_LICENSES` into `/usr/share/licenses/`. The
   installer does not do this: that file sits at the top level of the zip,
   outside the `dist/` directory the installer copies, so an unassisted install
   leaves it behind in the scratch directory and the image carries none of it.
5. Verifies, and fails the build rather than shipping something that merely looks
   installed: `aws --version` must report the pinned version, `/usr/bin/aws` must
   resolve to a path under `/usr/lib/aws-cli`, and `/usr/local` must be
   untouched. That last check is the assertion the fragment exists to make.
6. Removes `unzip` if it installed it, along with the dnf caches and logs.

All of that is one script on purpose. The tool bind-mounts `hooks/` for the
duration of a single `RUN`, and a bind mount is never committed to a layer, so
the 70 MB zip contributes zero bytes to the finished image. `unzip` is installed
and removed inside that same `RUN` for the same reason: had it been declared as
`packages.required` in `fragment.toml`, it would have been installed during the
earlier batched `dnf` step, and removing it later would write a whiteout while
the bytes stayed recoverable in that earlier layer forever.

### Why not extract without `unzip`

The base ships `python3`, and `python3 -m zipfile -e` needs nothing installed.
It also does not preserve the execute bit: the zip records `aws/install` and
`aws/dist/aws` as `-rwxr-xr-x`, and Python's `zipfile` extracts them
`-rw-r--r--` (checked 2026-08-04). Recovering from that means `chmod`-ing the
paths the installer happens to need, which is a worse thing to maintain than
installing a 186 KB package and removing it in the same layer.

## Base image compatibility

The fragment asks almost nothing of the base: a package manager, and either
`unzip` or the ability to install it. The AWS CLI bundle is self-contained,
carrying its own Python and its own shared libraries, so there is no dependency
on the base's Python or its version.

The example manifest deliberately does not pin the base by digest. Pin by digest
if you want byte-reproducible rebuilds.

## Maintenance is your rebuild cadence

In image mode, keeping the CLI current stops being a thing you do to machines
and becomes a property of how often you rebuild. Each rebuild pulls a newer base
and reinstalls the pinned CLI into `/usr`, wholesale, with no upgrade path to go
wrong: precisely what the `/var/usrlocal` symlink would have taken away.
The only deliberate maintenance event left is bumping the fragment's pinned
version and re-recording its digests, a one-line change to a versioned artifact.
The consuming Containerfile never changes at all.

## License

The AWS CLI v2 is Apache-2.0
([`aws/aws-cli/LICENSE.txt`](https://github.com/aws/aws-cli/blob/develop/LICENSE.txt),
checked 2026-08-04), so the zip travels as hook material with no redistribution
question. The bundle ships a `THIRD_PARTY_LICENSES` file covering its vendored
dependencies, and the entrypoint installs it to
`/usr/share/licenses/awscli-zip/THIRD_PARTY_LICENSES` so it reaches recipients of
the built image rather than being discarded with the scratch directory.

AWS also publishes a detached PGP signature alongside each zip
(`<url>.sig`), verifiable against the public key in the AWS CLI installation
documentation, if you would rather establish the recorded digests yourself than
trust the table in `fetch-awscli-zip.sh`.

## Out of scope

A fragment that installs `awscli2` from AppStream would be the sane counterpart
to this one, and the two would conflict: both provide `/usr/bin/aws`, and the RPM
owns that path. No such fragment exists yet, so this one declares no
`conflicts.fragments` rather than naming something imaginary.
