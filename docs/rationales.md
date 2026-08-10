# Design Rationales

Engineering decisions behind osfragment-assemble's fragment format and assembly model. For the design argued as one case, front to back, start with [the design explainer](design.md); this document is the fine-grained decision record behind it.

## Why fragments are standard OCI images

Fragments use the existing OCI distribution stack: no new builder, no custom registry plugins, no client-side tooling changes. Vendors can publish fragments to quay.io, GHCR, or ECR using the same workflows they use for container images. Customers can pull them with skopeo, mirror them with podman, and scan them with existing supply chain tools.

The core argument now lives in [the design explainer](design.md): being an artifact rather than a piece of text is the point, a digest to pin, a tag to version, something to sign, something for a mirror to hold.

Alternative considered: a custom archive format (`.tar.gz` or `.zip`). Rejected because it requires a separate distribution story and doesn't benefit from existing registry infrastructure.

Alternative considered: an include directive in the Containerfile itself. Rejected on the distribution grounds above, and it would leave the composition work undone regardless: a unit's content has to be split across the package install, and batching, deduplication, and conflict detection are properties of the whole set rather than of any one inclusion site.

## Why fragments are not delivered as RPMs

The strongest alternative to a new artifact type is the one the ecosystem already has. A vendor could deliver each unit as packages: a release-style RPM carrying the repo definition and signing key, a second RPM carrying configuration and service presets, one requiring the other, the whole thing installed with a single `dnf` line. The pattern is decades old and every RPM user understands it.

Two things break, and each breaks at the boundary between package management and image assembly rather than inside package management itself.

**A third-party repository cannot bootstrap itself cleanly.** A release package has to be installed from somewhere, and no distribution carries third-party vendors' release packages in its own repositories. The first install always happens out of band, against a bare URL, with a signing key the system does not yet trust. A fragment is pulled from a registry like any other image: pinnable by digest, mirrorable, and scannable with the same supply chain tooling as everything else in the build.

**An installed package stays installed.** A unit delivered as an RPM becomes permanent inventory in the image it configures, because removing a package removes the files it delivered. Fragment payload lands as plain files with no installer attached, and hooks execute through a bind mount that leaves nothing behind; the reasoning in "Why build inputs stay out of the image" applies to the delivery mechanism itself.

And the composition problem is still unsolved: even with every unit packaged, someone hand-writes the Containerfile that stitches the units together, and for on-cluster builds the MachineOSConfig around it. Generating those is the tool's actual job. Delivering units as RPMs would not remove the need for the tool; it would move the unit metadata somewhere the build cannot see it.

## Why the fragment format is as light as possible, and no simpler

The design goal for `fragment.toml` is to be the *minimum* metadata that lets reusable units be muxed together, and deliberately nothing more.

### As light as possible: the muxer knows what a unit is, not what it does

Declarative languages for configuring Linux systems already exist and are mature: kickstart, osbuild blueprints, rpm-ostree treefiles, comps groups, cloud-init, Butane, and Ansible roles. The format defers to them completely. Configuration is their job, and they do it well.

`fragment.toml` has a different job: describing a unit so units can be combined. It carries `name`, `version`, `description`, `vendor`, `conflicts`, `provides`, and one deliberate exception, `packages.required`: the packages a fragment installs in order to be itself (see "Packages: what a unit *is* versus what a build *selects*" below). Every field but the last answers a question about the *unit*: what is this thing, who published it, what version is it, what can it not be combined with, when does it apply relative to other units. None of them, `packages.required` included, answers a question about the *system's configuration*: no users, no services, no firewall rules, no partitioning, no file contents. The format has no vocabulary for expressing arbitrary system state and does not need one. The forced package list describes the unit's own completeness, not the machine it lands on.

The model is an OCI annotation set: flat, typed facts about an artifact, readable by tooling that never looks inside it, which is why the fields map cleanly onto the `com.github.marrusl.osfragment.*` annotations. Both describe the artifact, not the machine. One practical consequence: a fragment that publishes its annotations is transparent from the outside. `skopeo inspect` on such a fragment shows what it is, who ships it, which repos it provides, and which packages it forces, without pulling or unpacking anything; conflict declarations live only in the in-layer `fragment.toml` and are read when the fragment is loaded. The ecosystem has no shortage of formats for describing what a Linux system should be; `fragment.toml` avoids becoming another one by having no vocabulary to say it in.

The format resolves no dependencies, enforces no version constraints, and runs no install transactions; there is no `requires` field at all, because the one tag that would turn unit metadata into a dependency system is deliberately absent. dnf owns package management in full, at build time, exactly as it always has, and every package name a manifest or a fragment declares is passed to it untouched. `conflicts.fragments` and `provides.repos` are a composition check between build units: does one fragment name another as incompatible, and do two fragments that provide the same repo agree about it. The check runs at generation time, before anything builds or installs, and the fields hold bare names, with no versions, no expressions, and no capability namespace. What the format captures is the metadata a Containerfile author is already holding in their head when they hand-assemble repo files, package names, and config snippets from three vendors' READMEs; `fragment.toml` writes that down once instead of re-deriving it on every build.

The vocabulary also has a narrow audience. A composer assembling an image never opens `fragment.toml`; their entire surface is the manifest, a few lines naming a base image, the fragments to compose, and, per fragment, optionally packages to select. The unit metadata is read and written by fragment authors, who are packaging engineers publishing a unit for others to consume, and for whom name, version, provides, and conflicts are the working vocabulary of their trade.

Actual system configuration lives in the payload, in formats that already exist. `tree/` carries config files verbatim. `hook/` runs real binaries with their own CLIs and config files. A fragment is free to carry a kickstart file, a blueprint, or a cloud-init config and invoke the appropriate interpreter from a hook; the assembly tool never parses those files and holds no opinion about them. Whichever declarative format a shop has standardized on keeps working; a fragment is how it gets packaged, versioned, and distributed.

### And no simpler: below this floor, the concept fails

Each remaining field earns its place. Remove any one and reusable units stop working:

- **Identity and versioning** (`name`, `version`): without them a unit cannot be published to a registry, referenced, pinned, or upgraded. There is nothing to depend on.
- **Conflicts and provides** (`conflicts.fragments`, `provides.repos`): without them, two fragments that supply the same repo or occupy the same role fight silently and the failure surfaces at runtime, on the deployed system, rather than at build time.

The zero-format alternative already exists and is what this tool is a response to: prose install instructions and copy-paste Containerfile snippets. That approach has no identity, no versions, no ordering guarantees, and no conflict detection. It does not compose, it cannot be updated in place, and every consumer re-derives the same integration by hand. Those gaps are the reason for the format; the Containerfile itself is not the problem, and it remains the language everything here generates down to.

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

This is a hard boundary, and it is worth being precise about why. A flat list is a statement of fact about the unit: these packages are part of what this fragment is. Add conditionals (per-architecture names, per-base-image variants, install-only-if-another-fragment-is-present) and the field acquires an evaluation order and a context to evaluate against. At that point the field is on its way to becoming a DSL, with everything that entails.

Hooks already cover this case, and cover it better than growing `fragment.toml` a conditional syntax would. A hook is a real executable that can inspect the system and decide. Logic belongs in a language that has logic. A fragment needing different packages on different architectures is either two fragments or a hook; both are clear, and neither asks the format to grow. That choice has a real cost, covered below in "Why manifest-declared packages are preferred over hook-installed packages".

Keeping the list flat is what keeps `fragment.toml` readable at a glance, diffable, and mechanically checkable.

### The decision test

Two questions settle whether something belongs in `fragment.toml`:

1. **Does it describe the fragment, or the system the fragment configures?** The first belongs in `fragment.toml`. The second belongs in `tree/` or `hook/`.
2. **If it describes the fragment, is it a flat statement of fact, or does it need to be evaluated?** Facts belong in `fragment.toml`. Anything requiring conditions, precedence, or context belongs in a hook.

## What a fragment is for depends on who is writing it

The same format serves two people with different problems, and the parts they use barely overlap.

**A vendor's problem is that there is no way to ship the thing.** Today a vendor documents an integration in prose and the reader retypes it. What a vendor packages into a fragment is a repo definition, the package or packages their component needs, and the hooks that set it up: one artifact, versioned and published, that a consumer references by name instead of transcribing. Their configuration defaults usually arrive inside the package already, so a vendor's `tree/` is often thin or empty.

**A consumer's problem is combining several additions into one image.** For them `tree/` is the important part. It is where their own configuration lives, the settings that differ from whatever a package ships by default, and where they override what a vendor's fragment provides. This is why configuration lands after the package install: it is what makes the configuration a consumer authored the version that survives into the image.

A useful consequence: a fragment is also a base image. Deriving from a vendor's fragment, layering your own files on top, and publishing the result as your own fragment is an ordinary container build (COPY-only: a fragment is built from scratch, so there is no shell for a `RUN` to use), and it is how a consumer keeps their overrides while still tracking a vendor's updates.

## How fragments compose with configuration management

Configuration management describes what a system should look like. Ansible roles, system roles built on them, and the declarative formats listed above are all mature answers to that question, and a fragment has no vocabulary to compete with any of them.

A fragment answers a different question: how a reusable piece of an image gets published, versioned, and placed in a build. The two compose at the payload boundary, in either direction. A fragment can carry a playbook and run it from a hook at build time. Or a playbook can run wherever it already runs and its output can be captured into `tree/` and shipped as a fragment. In both cases the configuration logic stays where it is, written in the language it is already written in, and what the fragment adds is distribution and placement: a versioned artifact a third party can publish, and a defined position relative to the package install.

## Why conflicts resolve by order rather than by merging

Two fragments can write the same path. The rule is that fragments apply in manifest order and the last writer wins, and the generated Containerfile lists every such path in its header comment (standalone output; `--ocp` output carries no comments) so the outcome is visible rather than discovered later.

Ordering is deliberately the whole contract. Anything smarter, merging by key or by section or by line, requires the tool to understand what a given file *is*, and that is an unbounded commitment: every format anyone might ship would eventually need its own merge behavior. The tool would become an arbiter of content, which is precisely the job it declines everywhere else in this document.

Two escape hatches cover what ordering does not. Genuinely additive resources belong in `.d`-style directories, which is the host's own convention for the problem and needs nothing from this format. Anything else is a derived fragment: take the fragment you disagree with as a base, change what you need, and publish your own. Same mental model as container layering, which anyone composing images already has.

Repo definitions are the deliberate exception, and they fail the build instead. Two fragments disagreeing about what a repository is cannot be resolved by picking one, because the disagreement itself is the defect.

## Why no fragment type taxonomy

All fragments follow the same format (`fragment.toml`, `tree/`, `hook/`). There is no type system distinguishing "repo fragments" from "config fragments" from "service fragments", and no field declaring which a unit is. A fragment that installs a repo is structurally identical to one that drops a config file; what separates them is the paths they carry, which the generator reads directly.

This keeps the format simple and avoids artificial constraints: a fragment can deliver repo definitions, config files, and hooks in a single unit when that's the right packaging boundary. Hooks are not limited to configuration, either; a fragment can ship a hook that validates the result of its own composition.

## Why the tool generates Containerfiles instead of building directly

The tool deliberately stays out of the builder's domain. It doesn't invoke podman, doesn't wrap buildah, doesn't manage layers, doesn't handle multi-arch, doesn't implement remote builds. All of those are problems that container build tooling already solves. Generating a Containerfile means those solutions keep working.

This is the same deference the format section applies to configuration languages and the packages rationale applies to dnf, extended to the builder: three domains with competent owners, and the tool generates for all three rather than replacing any of them.

The generated Containerfile is a build artifact operators can read, edit, and version. If osfragment-assemble stops meeting their needs, they take the Containerfile and maintain it manually. The tool's job is codegen, not gatekeeping.

The composition problem could also be solved with a build DSL, a custom BuildKit frontend, or a builder daemon. Each of those is more capable than codegen, and each is a new component a build pipeline would have to adopt, trust, and debug. Generating a plain Containerfile means the fragment format is the only new thing here; everything downstream of it runs on build tooling users already have. Most of the emitted syntax is base Containerfile; hook execution uses `RUN --mount`, a BuildKit/Buildah extension that podman, buildah, and current Docker all support (see "Why build inputs stay out of the image" below).

## Why manifest-declared packages are preferred over hook-installed packages

Packages declared in the manifest's `packages:` field are batched into a single `dnf install` layer and deduplicated across fragments. This gives the tool visibility into what's being installed and keeps the generated Containerfile predictable.

Hooks that call `dnf` or run vendor installers (like NVIDIA's `.run` binaries) are not prevented; the tool doesn't enforce this. But packages installed by hooks bypass deduplication, won't appear in the manifest's package list, and cost their own `RUN --mount` layer per fragment instead of sharing the one batched install. This is the flip side of the guardrail above: hooks are the better tool for conditional installs, and giving up dedup and manifest visibility is the deliberate price of keeping the packages list flat instead of teaching it conditionals. When possible, prefer the manifest; when a hook genuinely needs to install packages (vendor installers, complex dependency chains), that's fine.

## Why digest pinning is opt-in

Digest pinning makes the fragment inputs to a build exact, but it makes the generated Containerfile harder to read and breaks registry mirrors that don't preserve digests. The default (`--pin-digests` omitted) uses tags, which are more human-friendly and work with disconnected/airgap scenarios where images are re-pushed to internal registries.

Pinning covers fragment references. Package resolution happens at build time against live repositories, exactly as it does in any Containerfile that installs packages, so that behavior is inherited from the build model rather than introduced here. Where bit-identical rebuilds matter, the levers sit one layer down: pinned repositories or a snapshot mirror for the package set, and the builder's own deterministic settings for timestamps and image IDs. Both already exist, and neither needs anything from this tool.

Pinning also changes the emitted form. Digests are long and unreadable, so the tool switches to named stages to keep pinned Containerfiles reviewable; unpinned refs are short enough to inline:

Unpinned: `COPY --from=quay.io/example/fragment:1.0 /fragment/tree/ /`  
Pinned: `FROM quay.io/example/fragment@sha256:... AS frag-example` then `COPY --from=frag-example /fragment/tree/ /`

## Why the tool cleans up dnf artifacts in the same RUN layer

The `RUN dnf install ... && dnf clean all && rm -rf /var/log/dnf*` pattern runs in a single layer to prevent dnf metadata from inflating the image size. If cleanup ran in a separate `RUN`, the previous layer would still carry the full dnf cache even though it's deleted in the next layer.

This is standard Containerfile practice, not specific to osfragment-assemble.

## Why `FROM configs AS final` for OCP mode

OpenShift's on-cluster build system uses `FROM configs AS final` to mark the stage MCO should extract. The base image and fragment stages are build-time dependencies only; the `final` stage is what gets deployed to nodes.

Standalone mode uses `FROM <base-image>` because there's no special stage marker needed outside the MCO context.

## Why config files land after packages

A unit's content does not sit in one place in the build. Repo definitions have to land before the package install so the packages are reachable, configuration has to land after it, and hooks run after that. There is no single point in a Containerfile where a unit can be inserted whole.

The ordering exists because more than one author writes files that end up at the same paths. A package carries its own defaults, and whoever assembles the image has configuration of their own to lay down on top of them. During RPM installation, a package that installs a file already present on disk and not marked `%config` overwrites it. If configuration were copied before the install, the package defaults would silently replace it, the build would still succeed, and the difference would appear only on a running system. Copying configuration after the install is what makes the authored configuration the version that survives.

Between fragments the rule is different and deliberately simple: fragments apply in manifest order and identical paths resolve last-writer-wins, so a later fragment can intentionally override an earlier one. The generated Containerfile lists every path written by more than one fragment in its header comment (standalone output; `--ocp` output carries no comments), so the resolution is visible in the artifact rather than implied.

## Why build inputs stay out of the image

Hooks are build inputs, not delivered payload. The tool emits `RUN --mount=type=bind` to execute hooks without copying their bytes into the image:

```dockerfile
RUN --mount=type=bind,from=<fragment>,source=/fragment/hook,target=/frag-hook,z \
    /frag-hook/entrypoint
```

The bind mount exists only during the `RUN` instruction. Hook scripts execute, produce their effects (install packages, write config files, enable services), and disappear. Nothing from `/fragment/hook` persists in the final image layers.

`tree/` content is the opposite case: it is delivered payload and is `COPY`'d into the image, where it correctly persists in the layer history.

The distinction matters because build inputs are not runtime artifacts. Hook bytes in image layers are recoverable via layer inspection tools, and shipping build tooling in a production image is the container equivalent of shipping your Makefile in a binary release. The scripts ran once at build time and are dead weight afterward.

Alternative considered: copy hooks, execute them, and remove them in the same `RUN` layer. Rejected because the remove step only writes a whiteout tombstone; the hook bytes remain in the layer and are recoverable. The bind mount avoids this entire class of problem: one instruction, one layer, nothing committed, nothing to clean up.

## Trust boundary

Fragments are trusted build code, not passive data. A fragment's hooks run as root during image assembly. Pulling a fragment is equivalent to running an upstream install script; it's a supply chain trust decision. The tool does not sandbox fragment hooks or validate their behavior.

Repo deduplication is a correctness check, not a security boundary. When two fragments ship a repo file with the same repo ID and identical content, one copy is kept. When the contents differ, generation fails with an error naming both fragments; the tool never silently picks a winner between two units that disagree about what a repository is. How every other kind of collision resolves is covered in "Why conflicts resolve by order rather than by merging" above, and none of it is a security mechanism either.
