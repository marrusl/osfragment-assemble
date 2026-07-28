# Design Rationales

Engineering decisions behind osfragment-assemble's fragment format and assembly model.

## Why fragments are standard OCI images

Fragments use the existing OCI distribution stack: no new builder, no custom registry plugins, no client-side tooling changes. Vendors can publish fragments to quay.io, GHCR, or ECR using the same workflows they use for container images. Customers can pull them with skopeo, mirror them with podman, and scan them with existing supply chain tools.

Alternative considered: a custom archive format (`.tar.gz` or `.zip`). Rejected because it requires a separate distribution story and doesn't benefit from existing registry infrastructure.

## Why the fragment format is as light as possible, and no simpler

The design goal for `fragment.toml` is to be the *minimum* metadata that lets reusable units be muxed together, and deliberately nothing more.

### As light as possible: the muxer knows what a unit is, not what it does

Declarative languages for configuring Linux systems already exist and are mature: kickstart, osbuild blueprints, rpm-ostree treefiles, comps groups, cloud-init, Butane. The format defers to them completely. Configuration is their job, and they do it well.

`fragment.toml` has a different job: describing a unit so units can be combined. It carries `name`, `version`, `description`, `vendor`, `phase`, `conflicts`, `provides`, and one deliberate exception, `packages.required`: the packages a fragment forces to install to be itself (see "Packages: what a unit *is* versus what a build *selects*" below). Every field but the last answers a question about the *unit*: what is this thing, who published it, what version is it, what can it not be combined with, when does it apply relative to other units. None of them, `packages.required` included, answers a question about the *system's configuration*: no users, no services, no firewall rules, no partitioning, no file contents. The format has no vocabulary for expressing arbitrary system state and does not need one. The forced package list describes the unit's own completeness, not the machine it lands on.

The model is an RPM spec header or an OCI annotation set. `Name:`, `Version:`, `Provides:`, and `Conflicts:` are how a packaging system identifies a unit and reasons about combining it with other units, and `fragment.toml` borrows that identifying vocabulary, not the machinery behind it: no resolver, no dependency graph, no transaction log. Its fields map cleanly onto the `io.bootc.fragment.*` OCI annotations for the same reason: both describe the artifact, not the machine. The ecosystem has no shortage of formats for describing what a Linux system should be; `fragment.toml` avoids becoming another one by having no vocabulary to say it in.

None of this makes the format a package manager, and it is worth stating plainly what it refuses to do. It resolves no dependencies, enforces no version constraints, and runs no install transactions; dnf owns package management in full, at build time, exactly as it always has. `conflicts.fragments` and `provides.repos` are a composition check between build units (do two fragments claim the same repo, or the same role, in an image), a granularity package managers do not operate at and were never built to answer. What the format captures is the metadata a Containerfile author is already holding in their head when they hand-assemble repo files, package names, and config snippets from three vendors' READMEs; `fragment.toml` writes that down once instead of re-deriving it on every build.

Actual system configuration lives in the payload, in formats that already exist. `tree/` carries config files verbatim. `hooks/` runs real binaries with their own CLIs and config files. A fragment is free to carry a kickstart file, a blueprint, or a cloud-init config and invoke the appropriate interpreter from a hook; the assembly tool never parses those files and holds no opinion about them. Whichever declarative format a shop has standardized on keeps working; a fragment is how it gets packaged, versioned, and distributed.

### And no simpler: below this floor, the concept fails

Each remaining field earns its place. Remove any one and reusable units stop working:

- **Identity and versioning** (`name`, `version`): without them a unit cannot be published to a registry, referenced, pinned, or upgraded. There is nothing to depend on.
- **Ordering** (`phase`): repo definitions must land before packages install, and configuration must land after, or RPM defaults silently overwrite fragment-supplied files. Without a declared ordering the composition is a coin flip.
- **Conflicts and provides** (`conflicts.fragments`, `provides.repos`): without them, two fragments that supply the same repo or occupy the same role fight silently and the failure surfaces at runtime, on the deployed system, rather than at build time.

The zero-format alternative already exists and is what this tool is a response to: prose install instructions and copy-paste Containerfile snippets. That approach has no identity, no versions, no ordering guarantees, and no conflict detection. It does not compose, it cannot be updated in place, and every consumer re-derives the same integration by hand. Its failure is the reason for the format.

So the floor is: enough metadata to identify a unit and reason about combining it with other units. Everything above that floor belongs to formats that already exist.

### Packages: what a unit *is* versus what a build *selects*

Packages are the interesting case, because a package list can be either kind of thing depending on who wrote it and why. The format splits them accordingly.

**A fragment forces the packages it needs in order to be itself.** A Grafana fragment that ships a repo definition, default configuration, and service enablement but does not install `grafana` is not a Grafana fragment; it is a pile of parts with a README. Forcing the package makes the fragment a complete, canonical install: a vendor publishes one artifact, a consumer references it, and the result is a working service. That is the whole point of a reusable unit, and it is squarely "what this unit is." It belongs in `fragment.toml`.

**A manifest selects packages ad hoc across repos.** `packages: [htop, tmux, jq]` against a bare EPEL fragment is not describing a unit; it is cherry-picking from a repository that happens to be reachable. Those are dnf arguments, and a list of dnf arguments is not a language. They belong at the composition site, where the person doing the composing decides what this particular image needs.

The two are complementary, and the split falls out of the same test as everything else. A pure content repository like EPEL forces nothing: it provides a repo, and what you take from it is your business. An opinionated fragment forces exactly what it takes to deliver the thing it claims to deliver. Both are expressible, neither requires new syntax, and package lists that define a unit stop being copy-paste instructions in someone's documentation.

Declaring forced packages is optional, and enumeration is never required. A fragment lists the packages it must install to be itself: typically a handful, often one. A fragment that exists to provide a repository lists nothing at all: enumerating a repository's contents would be impossible for anything the size of EPEL and pointless at any size, since dnf resolves names at build time. Manifest selection is likewise unconstrained by what a fragment declares; the muxer orders and batches package installs without needing to know what a repository contains.

Alternative considered: fragment-side *defaults* that the manifest can override, or separate "package set" fragments. Rejected because both relocate the list rather than remove it, and both add a resolution step (precedence rules, override semantics) that the flat split does not need.

### The guardrail: forced packages stay a flat list

Forced packages are a flat list of names. Not a map, not conditionals, no `when:` keys, no per-architecture or per-base-image variants.

This is a hard boundary, and it is worth being precise about why. A flat list is a statement of fact about the unit: these packages are part of what this fragment is. Add conditionals (per-architecture names, per-base-image variants, install-only-if-another-fragment-is-present) and the field acquires an evaluation order and a context to evaluate against. At that point it is a small programming language, and it needs the tooling every language needs.

Hooks already cover this case, and cover it better than growing `fragment.toml` a conditional syntax would. A hook is a real executable that can inspect the system and decide. Logic belongs in a language that has logic. A fragment needing different packages on different architectures is either two fragments or a hook; both are clear, and neither asks the format to grow. That choice has a real cost, covered below in "Why manifest-declared packages are preferred over hook-installed packages"; paying it is deliberate, not an oversight.

Keeping the list flat is what keeps `fragment.toml` readable at a glance, diffable, and mechanically checkable.

### The decision test

Two questions settle whether something belongs in `fragment.toml`:

1. **Does it describe the fragment, or the system the fragment configures?** The first belongs in `fragment.toml`. The second belongs in `tree/` or `hooks/`.
2. **If it describes the fragment, is it a flat statement of fact, or does it need to be evaluated?** Facts belong in `fragment.toml`. Anything requiring conditions, precedence, or context belongs in a hook.

## Why no fragment type taxonomy

All fragments follow the same format (`fragment.toml`, `tree/`, `hooks/`). The `phase` field controls ordering, but there's no type system distinguishing "repo fragments" from "config fragments" from "service fragments". A fragment that installs a repo is structurally identical to one that drops a config file.

This keeps the format simple and avoids artificial constraints: a fragment can deliver repo definitions, config files, and hooks in a single unit when that's the right packaging boundary.

## Why the tool generates Containerfiles instead of building directly

The tool deliberately stays out of the builder's domain. It doesn't invoke podman, doesn't wrap buildah, doesn't manage layers, doesn't handle multi-arch, doesn't implement remote builds. All of those are problems that container build tooling already solves. Generating a Containerfile means those solutions keep working.

This follows the same principle documented in the format section: declarative languages for configuring Linux systems already exist and are mature, so the format defers to them completely. The tool applies the same principle to builders. It applies it to package managers too. The packages rationale already says "those are dnf arguments, and a list of dnf arguments is not a language."

The pattern is: the tool occupies the composition layer and stays out of domains that have competent owners. Builder, configurator, package manager. The tool generates for all three rather than replacing any of them.

The generated Containerfile is a build artifact operators can read, edit, and version. If osfragment-assemble stops meeting their needs, they take the Containerfile and maintain it manually. The tool's job is codegen, not gatekeeping.

The composition problem could also be solved with a build DSL, a custom BuildKit frontend, or a builder daemon. Each of those is more capable than codegen, and each is a new component a build pipeline would have to adopt, trust, and debug. Generating a plain Containerfile means the fragment format is the only new thing here; everything downstream of it runs on build tooling users already have. Most of the emitted syntax is base Dockerfile; hook execution uses `RUN --mount`, a BuildKit/Buildah extension that podman, buildah, and current Docker all support (see "Why build inputs stay out of the image" below).

## Why manifest-declared packages are preferred over hook-installed packages

Packages declared in the manifest's `packages:` field are batched into a single `dnf install` layer and deduplicated across fragments. This gives the tool visibility into what's being installed and keeps the generated Containerfile predictable.

Hooks that call `dnf` or run vendor installers (like NVIDIA's `.run` binaries) are not prevented; the tool doesn't enforce this. But packages installed by hooks bypass deduplication, won't appear in the manifest's package list, and cost their own `RUN --mount` layer per fragment instead of sharing the one batched install. This is the flip side of the guardrail above: hooks are the better tool for conditional installs, and giving up dedup and manifest visibility is the deliberate price of keeping the packages list flat instead of teaching it conditionals. When possible, prefer the manifest; when a hook genuinely needs to install packages (vendor installers, complex dependency chains), that's fine.

## Why digest pinning is opt-in

Digest pinning guarantees reproducibility but makes the generated Containerfile harder to read and breaks registry mirrors that don't preserve digests. The default (`--pin-digests` omitted) uses tags, which are more human-friendly and work with disconnected/airgap scenarios where images are re-pushed to internal registries.

When digests are pinned, the tool switches to named stages (`AS frag-<name>`) for readability; otherwise it uses inline `COPY --from=<image-ref>` to keep the Containerfile compact.

## Why the tool cleans up dnf artifacts in the same RUN layer

The `RUN dnf install ... && dnf clean all && rm -rf /var/log/dnf*` pattern runs in a single layer to prevent dnf metadata from inflating the image size. If cleanup ran in a separate `RUN`, the previous layer would still carry the full dnf cache even though it's deleted in the next layer.

This is standard Containerfile practice, not specific to osfragment-assemble.

## Why `FROM configs AS final` for OCP mode

OpenShift's on-cluster build system uses `FROM configs AS final` to mark the stage MCO should extract. The base image and fragment stages are build-time dependencies only; the `final` stage is what gets deployed to nodes.

Standalone mode uses `FROM <base-image>` because there's no special stage marker needed outside the MCO context.

## Why inline image refs by default, named stages when pinning

Unpinned: `COPY --from=quay.io/example/fragment:1.0 /fragment/tree/ /`  
Pinned: `FROM quay.io/example/fragment@sha256:... AS frag-example` then `COPY --from=frag-example /fragment/tree/ /`

Digests are long and unreadable. Named stages make pinned Containerfiles easier to review. Unpinned refs are short enough to inline without harming readability.

## Why config files land after packages

Fragment config files are copied to the target image after package installation completes. This guarantees that fragment configurations always win when they overlap with RPM-installed files.

During RPM installation, if a package installs a file that already exists on disk and isn't marked as `%config` in the RPM spec, the RPM will overwrite the existing file. If fragment configs were copied before packages, RPM installs could silently replace them with package defaults. Copying configs after package installation ensures fragment-supplied configurations are never overwritten; the intended state from the fragment is what lands in the final image.

## Why build inputs stay out of the image

Hooks are build inputs, not delivered payload. The tool emits `RUN --mount=type=bind` to execute hooks without copying their bytes into the image:

```dockerfile
RUN --mount=type=bind,from=<fragment>,source=/fragment/hooks,target=/frag-hooks,bind-propagation=rshared,z \
    /frag-hooks/10-configure.sh && /frag-hooks/20-enable.sh
```

The bind mount exists only during the `RUN` instruction. Hook scripts execute, produce their effects (install packages, write config files, enable services), and disappear. Nothing from `/fragment/hooks` persists in the final image layers.

`tree/` content is the opposite case: it is delivered payload and is `COPY`'d into the image, where it correctly persists in the layer history.

The distinction matters for two reasons. First, build inputs are not runtime artifacts. Hook bytes in image layers are recoverable via layer inspection tools, and shipping build tooling in a production image is the container equivalent of shipping your Makefile in a binary release. Second, at fleet scale, hook scripts that land in every node's image inflate pull size without delivering runtime value. The scripts ran once at build time and are dead weight afterward.

Alternative considered: copy hooks, execute them, and remove them in the same `RUN` layer. Rejected because the remove step only writes a whiteout tombstone; the hook bytes remain in the layer and are recoverable. The bind mount avoids this entire class of problem: one instruction, one layer, nothing committed, nothing to clean up.

## Trust boundary

Fragments are trusted build code, not passive data. A fragment's hooks run as root during image assembly. Pulling a fragment is equivalent to running an upstream install script; it's a supply chain trust decision. The tool does not sandbox fragment hooks or validate their behavior.

Repo deduplication prevents different fragments from silently overwriting each other's repos, but it's not a security boundary; if two fragments provide the same repo with different content, the build fails rather than choosing one.
