# osfragment-assemble Design Overview

A condensed version of [the design explainer](design.md), which gives each point its full argument.

osfragment-assemble turns fragments, published, versioned units of integration knowledge, into bootc image builds: a short manifest in, a plain Containerfile out.

## Worked out by hand, shipped nowhere

On bootc there is no path to follow. Vendor install documentation, where it exists, describes the conventional-system workflow; translating it into an image build is on you, and the derivation lives nowhere but your image and repeats for the next image, the next base bump, the next organization. Nothing is published, nothing carries a version a stranger can pin, and from vendors, not even prose.

A running system has drop-in directories, a place for a third party's piece; an image build has no equivalent. An application platform composes vendor containers at deploy time; a bootc host runs one image, nothing composes into it after it is built, every integration converges on the same Containerfile, and the knowledge behind each one is held by whoever last derived it.

## The unit is an image

A captured derivation must be publishable, or a second party can never receive it; versionable, to track the software it integrates; pinnable, so the fragment that worked yesterday is the same fragment today; signable, so trusting it is a decision instead of a hope; mirrorable, for disconnected environments; scannable, to fit existing supply chain tooling; and composable, because nobody integrates exactly one thing.

That list is exactly what a container registry already provides for images, and none of it is true of text, so the unit is a standard OCI image: an artifact, which is the point. And with this knowledge captured nowhere today, a first form gets to skip prose entirely.

One boundary: a one-line change stays a line in your Containerfile; a fragment captures what would otherwise be re-derived again and again.

## The mechanism

A fragment is an OCI image carrying four things. `fragment.toml` holds flat facts, on the model of an OCI annotation set: name, version, vendor, provides, conflicts, and the packages it needs in order to be itself. Every field describes the unit, none the machine it lands on. `tree/` is delivered payload: files copied into the image verbatim. `hook/` is build input, not payload: the tool runs one executable, `hook/entrypoint`, once, through a bind mount lasting only its build step; nothing of it persists in the image. `mount/` is build input too, not payload, but unlike `hook/` the tool never runs it: its files bind-mount onto the package install step and every hook step so package acquisition can authenticate (entitlement certs, mirror client certs, proxy CA bundles); the tool commits none of it, but hook code is trusted and could still copy it into a layer, so pair credential fragments only with hook fragments you trust. A fragment whose `mount/` derives mount points must be pinned by digest, since a moved tag could otherwise swap its trust material. The composing side is a few YAML lines: a base image and fragments, each optionally selecting packages or naming a mirror to rewrite its repo definitions against.

What earns the tool its keep is placement. A unit's content does not sit in one place in a build: its repo definition must land before the package install, or the packages are unreachable; its configuration must land after, or the package's defaults at the same paths silently replace it, a difference the build never reports and the running system does. Hooks run after that. So there is no correct paste position for an integration in a Containerfile: splicing a snippet in whole, at any single point, is quietly wrong.

The generated Containerfile therefore interleaves: repo files from every fragment, one batched and deduplicated package install, configuration, then hooks. It also catches what a human splicing snippets cannot see: identical repo definitions deduplicate, two fragments disagreeing about what a repository is fail generation, and declared conflicts stop generation before anything builds. Out comes a plain Containerfile, the tool's decisions readable in its build steps.

## Powerful because of what it refuses to own

Three refusals define the tool, each a domain handed in full to its existing owner.

dnf owns package management. Every declared package name passes through untouched; the tool carries no dependency logic. `provides` and `conflicts` are not a second dependency system but a composition check between build units, run at generation time, surfacing incompatibilities before a build rather than on a deployed system.

Configuration languages own configuration. The payload carries the files, a hook invokes the interpreter, and the tool parses none of it. There is no fragment-native way to declare a user, a service, a firewall rule, or a partition: the format has no vocabulary for system state, and that absence is the design.

Builders own building. The tool generates a Containerfile and stops; asked for more, it emits the MachineOSConfig wrapper for OpenShift on-cluster builds or a self-contained build context, still codegen, never a build. podman, buildah, and any builder supporting `RUN --mount`, the one extension hooks and build mounts both need beyond base Containerfile syntax, consume the output unchanged.

What the absences buy is proportion: a tool small enough to audit, a format readable at a glance, a trust surface that stays visible. What a fragment is, its metadata states; what a fragment does, its files show.

## What transparency buys

A published fragment answers most adoption questions from its annotations, which registry tooling reads without pulling anything, and inside there is nothing to decode: payload is stored verbatim, hooks are ordinary executables, and the one declared exception, a manifest-set mirror rewriting a fragment's repo definitions, is visible in both the manifest and the generated Containerfile. The format never wraps, encodes, or compiles anything; opacity, where it exists, is authored, not structural.

A fragment is also a base: deriving from one and layering your own files on top is an ordinary container build (COPY-only, since a fragment is built from scratch and carries no shell), and it is how you keep overrides while tracking the upstream unit's updates. The exit is open too: the generated Containerfile is a readable build artifact, so if the tool stops earning its place, take it and maintain it by hand; the job is codegen, not gatekeeping. And the authoring bar is low: flat facts plus files you already have, so your own derivation is most of the way to a fragment.

## Why this could spread

Everything the model depends on is infrastructure nobody owns: any registry, any bootc base, signing and scanning tools that answer to nobody in particular. A vendor publishing a fragment is not betting on another vendor's toolchain, and dependence on the tool stays shallow by construction.

And in one specific channel there is no incumbent to displace. A one-off change has an incumbent form, a line you write yourself; the form integration knowledge takes when shipped has none: no artifact, no format, no bootc-specific prose. Fragments can be that form from the start. The pattern can also grow from the bottom: consumers are producers from day one, a platform team's captured derivation is a published fragment before any vendor shows up, and a vendor's canonical fragment arriving later is one manifest reference changed, an upgrade rather than a migration.

The precedent is containers themselves: a neutral mechanism first, an ecosystem after. The bet is not that fragments become the only way anyone builds a bootc image; it is that shipped integration knowledge deserves a form, and that a standard OCI image, inspectable from the outside and readable on the inside, is the right first form for it to take.

---

The individual decisions behind this design, and the alternatives considered, are recorded in [Design Rationales](rationales.md).
