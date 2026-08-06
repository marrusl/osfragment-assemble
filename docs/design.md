# The Design of osfragment-assemble

osfragment-assemble turns fragments, published, versioned units of integration knowledge, into bootc image builds: a short manifest in, a plain Containerfile out. This document explains the design from the problem up: what is missing on bootc, what the missing thing has to be, and why the tool is built around refusals.

## Worked out by hand, shipped nowhere

On bootc there is no path to follow. A vendor's install documentation, where it exists, describes the conventional-system workflow: add this repository, install this package, edit that configuration, enable the service. Translating that into an image build is entirely on you. You work it out from what you know about RPM and how you want the software configured, you encode the result in your Containerfile, and that derivation lives nowhere but your image. The next team starts from zero and derives it again. So does the next image definition, and the next base bump. Nothing is published, nothing is versioned, nothing is shared. Not even prose.

Notice what is missing. A running system has drop-in directories, a place where a third party can put their piece without coordinating with anyone else. An image build has no equivalent, and nothing to drop into it.

For the reader who thinks in platforms, the pressure here is structural, not incidental. On an application platform, every vendor ships its own container and composition happens at deploy time. A bootc host runs one image, and nothing composes into it after it is built. Whatever belongs to the image itself, `/usr` and the packages that populate it, has to be present when the build finishes, so every integration, from every party, converges on the same Containerfile, and the knowledge behind each one is held by whoever last derived it.

## The unit is an image

What would it take to capture one of those derivations properly? Walk the requirements. The captured integration needs to be publishable, or a second party can never receive it. Versionable, so it can track the software it integrates. Pinnable, so the fragment that worked yesterday is the same fragment today. Signable, so trusting it is a decision instead of a hope. Mirrorable, so it survives disconnected environments. Scannable, so it fits the supply chain tooling already in place. And composable with others, because nobody integrates exactly one thing.

That list is exactly what a container registry already provides for images. So the unit is a standard OCI image, and everything a registry does for container images it does for fragments at no additional cost.

Being an artifact rather than a piece of text is the point. A unit that lives in a registry has a digest, so it can be pinned exactly. It has a tag, so it can be versioned and upgraded. It can be signed, mirrored into a disconnected environment, pulled through whatever authenticated proxy is already in place, and scanned by whatever already scans images. Text has none of that: no identity to pin, nothing to sign, nothing for a mirror to hold.

There is a quieter observation underneath. Because bootc integration knowledge is not captured anywhere today, the question was never how to improve on existing write-ups. The question is what form this knowledge should take when it is captured for the first time, and a first form gets to skip prose entirely and go straight to an artifact.

One boundary before going further: not every change wants a unit. A one-line change stays a line in your Containerfile; a fragment captures an integration that would otherwise be re-derived by the next team, the next image, the next base bump.

## The mechanism

A fragment is an OCI image carrying three things, each with a strict job.

`fragment.toml` holds flat facts about the unit, on the model of an OCI annotation set: name, version, a one-line description, vendor, what it provides, what it conflicts with, and the packages it needs in order to be itself. Every field describes the unit. None describes the machine it lands on; the format has no vocabulary for system state.

`tree/` is delivered payload: files copied into the image verbatim, laid out exactly as they will land on the target filesystem. Repo definitions, signing keys, configuration files, systemd presets.

`hooks/` is build input, not payload. A fragment that carries hooks names one executable, `hooks/entrypoint`, and that is the only thing the tool runs: once, through a bind mount that exists only for the duration of its build step, with everything else under `hooks/` riding along as support material for it. The hook does its work, and nothing of it persists in the image.

The composing side is smaller still. A manifest is a few lines of YAML naming a base image and the fragments to compose, with each fragment entry optionally selecting packages or naming a mirror to rewrite that fragment's repo definitions against. The `packages:` lists are dnf passthrough: every name is handed to dnf untouched, at build time.

What earns the tool its keep is what it does with a set of fragments, and the heart of it is a fact about placement. A unit's content does not sit in one place in a build. Its repo definition has to land before the package install, or the packages are unreachable. Its configuration has to land after the install: the package carries defaults at the same paths, and if your configuration went in first, the install could silently replace it. The build succeeds either way; the difference surfaces on the running system. Hooks run after that, against the assembled filesystem. So a single unit's repo definition and its configuration belong on opposite sides of the package install, which means there is no correct paste position for an integration in a Containerfile. Not at scale; not even for one unit. Splicing a snippet in whole, at any single point, is quietly wrong.

The generated Containerfile therefore interleaves: repo files from every fragment first, then a single batched package install, then configuration, then hooks. Along the way the tool detects what a human splicing snippets cannot see. Package installs are batched into one layer and deduplicated across fragments. Identical repo definitions are deduplicated, and generation fails when two fragments disagree about what a repository is, with an error naming both. Declared conflicts stop generation the same way, before anything builds or installs. Out the other end comes a plain Containerfile, with the tool's decisions readable in the build steps themselves.

## Powerful because of what it refuses to own

Three refusals define the tool, and each one is a domain handed, in full, to the thing that already owns it.

dnf owns package management. Every package name a manifest or a fragment declares is passed through untouched, and dnf does at build time what it has always done: dependency work, version selection, the install transaction. The tool carries no dependency logic of its own. The `provides` and `conflicts` fields in fragment metadata are not a second dependency system; they are a composition check between build units, run at generation time: `conflicts.fragments` names fragments a unit refuses to sit beside, and `provides.repos` names the repositories a unit supplies, so a declared incompatibility, or two fragments disagreeing about a repository they both provide, surfaces before a build starts rather than on a deployed system.

Configuration languages own configuration. Whatever a shop already uses keeps working: the payload carries the files, a hook invokes the interpreter, and the tool never parses any of it and holds no opinion about any of it. There is no fragment-native way to declare a user, a service, a firewall rule, or a partition, and that is not a gap awaiting a release. The format has no vocabulary for expressing system state, and that absence is the design.

Builders own building. The tool generates a Containerfile and stops; asked for more, it emits the MachineOSConfig wrapper for OpenShift on-cluster builds or a self-contained build context, and all of that is still codegen, never a build. podman and buildah consume the output unchanged, as does any builder that supports `RUN --mount`, the one extension beyond base Containerfile syntax that hook execution uses.

What the absences buy is proportion. The tool stays small enough to audit. The format stays readable at a glance, because flat facts about a unit are all it can say. And the trust surface stays visible: what a fragment is, its metadata states; what a fragment does, its files show. Each refusal is the same decision made three times. The domains bordering image assembly have competent owners already, and the tool composes their inputs while replacing none of them.

## What transparency buys

Start from the outside. A fragment published with its annotations answers most of the adoption questions before you pull it: what it is, who ships it, which repositories it provides, which packages it forces. Standard registry tooling reads those from the image's annotations without pulling or unpacking anything. Conflict declarations live in the `fragment.toml` inside the image, so checking those means pulling the fragment, and still only reading a short file. Either way, deciding whether a unit belongs in your image starts from the artifact's own declarations.

Inside, there is nothing to decode. Payload files are stored verbatim, so what you see in the fragment is byte for byte what lands in the image, with one declared exception: a manifest that sets a mirror on a fragment gets that fragment's repo definitions rewritten to point at the mirror, and the rewrite is visible both in the manifest that asked for it and in the generated Containerfile that performs it. Hooks are ordinary executables, typically scripts you just read. An author can ship an opaque binary in a hook, but the format never wraps, encodes, or compiles anything, so a fragment is exactly as transparent as its author chose to make it, and no less. Opacity, where it exists, is authored, not structural. Reviewing a fragment before composing it is reading a short TOML file and the files it ships: whatever tool you unpack it with, there is no format to decode on the other side.

The registry services follow for free: pin by digest, sign with the signing tools already in use, mirror into disconnected environments, scan with whatever already scans images. And because a fragment is an image, it is also a base. Deriving from a published fragment and layering your own configuration files on top is an ordinary container build, with one property to know: a fragment is built from scratch and carries no shell, so a derived build copies files in rather than running commands. That derivation is how you keep your overrides while tracking the upstream unit's updates, and no part of it is specific to this tool; it is container layering doing what it already does.

Two consequences round this out.

The exit is open. The generated Containerfile is a readable build artifact, not an opaque intermediate. If the tool stops earning its place, take the Containerfile and maintain it by hand; everything you built keeps building. The job here is codegen, not gatekeeping.

The authoring bar is low. A fragment is a TOML file of flat facts plus files you already have: the repo definition you already added, the configuration you already wrote, the setup script you already ran, installed as the hook's `entrypoint`. If you derived an integration yourself, you are most of the way to a fragment of it, without waiting for a canonical vendor fragment to exist.

## Why this could spread

Everything the model depends on is infrastructure nobody owns. Fragments live in any registry, compose onto any bootc base, and are signed and scanned by tools that exist today and answer to nobody in particular. A vendor who publishes a fragment is not betting on another vendor's toolchain, and between what is published and what is run, the tool itself is codegen anyone can walk away from. Dependence on it stays shallow by construction.

And in one specific channel there is no incumbent to displace. A one-off change in your own Containerfile has an incumbent form and always will: a line you write yourself. What has no incumbent is the form integration knowledge takes when it is shipped. That channel holds nothing today: no artifact, no format, no bootc-specific prose. Fragments do not have to convince anyone to abandon a working practice; they can be the form the shipping practice takes from the start.

The pattern can also grow from the bottom, because the authoring bar is low enough that consumers are producers from day one. A platform team that captures its own derivation as a fragment has already published one before any vendor shows up. When a vendor arrives later with a canonical fragment for the same software, swapping it in is one reference changed in a manifest, and the change is an upgrade, not a migration.

The precedent is containers themselves: a neutral mechanism first, an ecosystem after. The bet is not that fragments become the only way anyone builds a bootc image. The bet is that shipped integration knowledge deserves a form, and that a standard OCI image, inspectable from the outside and readable on the inside, is the right first form for it to take.

---

The individual decisions behind this design, and the alternatives considered along the way, are recorded in [Design Rationales](rationales.md).
