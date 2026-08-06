# Spec: Design explainer (docs/design.md)

**Status:** Proposed
**Date:** 2026-08-06

## Summary

Add `docs/design.md`, a narrative design explainer that becomes the repository's front-door document. `docs/rationales.md` remains as the fine-grained decision record. The README links design.md before anything else in docs/.

## Motivation

rationales.md is organized as standalone "Why X" entries. Each entry answers an objection; none builds the affirmative case. The reasoning is sound but the structure is defensive, and the strongest arguments (the registry as distribution infrastructure, transparency from the outside in) are buried mid-entry. The project is past its demo stage. The document that introduces it should explain the design as one coherent argument and make the case for why the model can spread.

## Audience

Adopters first: platform engineers and vendor packagers deciding whether to try the tool. The document's job is to make the model click and make them want it. Ecosystem readers second: the argument for why the pattern deserves to spread rides along for people who think in ecosystem terms.

## Register and style

- Mechanism-first, confident, concrete. No marketing vocabulary.
- The only alternative ever named is the status quo: working out an integration yourself and encoding it in a Containerfile by hand. No comparison with any named format, tool, or product. The wins are stated affirmatively and readers draw their own comparisons.
- Every claim about tool behavior must be true of the tool today. The one forward-looking section is clearly framed as such.
- No em dashes. Avoid the word "shape" (use "structure," "layout," "form," or name the thing directly).
- Target length: 2,000 to 2,300 words.

## Structure

Six sections, one arc. Titles below are indicative; the writer may improve them.

### 1. The problem (~250 words)

On bootc there is no path to follow. A vendor's install documentation, where it exists, describes the conventional-system workflow: add this repo, install this package, edit that config, enable the service. Translating that into an image build is entirely on the reader: you work it out from what you know about RPM and how you want the package configured, encode the result in your Containerfile, and that derivation lives nowhere but your image. The next team starts from zero and derives it again. Nothing is published, nothing is versioned, nothing is shared. Not even prose.

This status quo is the only foil the document ever names.

### 2. The missing thing is a unit (~300 words)

Derive the requirements: a captured integration needs to be publishable, versionable, pinnable, signable, mirrorable, scannable, and composable with others. Then the turn: that list is exactly what a container registry already provides for images. So the unit is a standard OCI image, and everything a registry does for container images it does for fragments at no additional cost.

Promote the core of the existing "Why fragments are standard OCI images" rationale here, including the argument that being an artifact rather than a piece of text is the point: a digest to pin, a tag to version, something to sign, something a mirror can hold.

Because bootc integration knowledge is not captured anywhere today, this section can also make a quieter point: the question is not how to improve on existing write-ups, it is what form this knowledge should take when it is captured for the first time. The answer gets to skip prose entirely and go straight to an artifact.

### 3. The mechanism (~500 words)

What a fragment is:

- `fragment.toml`: flat facts about the unit, on the model of an OCI annotation set. Name, version, vendor, what it provides, what it conflicts with, the packages it needs in order to be itself. No vocabulary for system state.
- `tree/`: delivered payload, copied into the image verbatim.
- `hooks/`: build inputs, executed through a bind mount, leaving nothing behind in the image.

What the tool does with a set of fragments: ordering (repo definitions before the package install, configuration after it, hooks after that), batching and deduplication of package installs, conflict detection at generation time before anything builds, and a plain Containerfile out the other end.

### 4. Powerful because of what it refuses to own (~400 words)

The design philosophy section, and a load-bearing one. Three deferences:

- dnf owns package management, in full, at build time.
- Existing configuration languages own configuration. The payload can carry whatever a shop already uses; the tool never parses it and holds no opinion.
- Builders own building. The tool generates a Containerfile and stops; podman, buildah, and every downstream pipeline keep working unchanged.

The format has no vocabulary for expressing system state, and that absence is the design. It is what keeps the tool small, the format readable at a glance, and the trust surface visible. This section is what makes the model feel inevitable rather than clever.

### 5. What transparency buys (~400 words)

Both stated wins live here, argued affirmatively.

From the outside: inspecting a published fragment shows what it is, what it forces, and what it conflicts with, without pulling or unpacking anything. Standard registry tooling answers the question.

From the inside: there is nothing to decode. Payload files are stored verbatim; hooks are ordinary executables, typically scripts you just read. An author can ship an opaque binary in a hook, but the format never wraps, encodes, or compiles anything, so a fragment is exactly as transparent as its author chose to make it, and no less. Opacity, where it exists, is authored, not structural.

The registry services follow for free: pin by digest, sign, mirror into disconnected environments, scan with whatever already scans images. Deriving from a published fragment and layering your own configuration on top is an ordinary container build, and it is how a consumer keeps overrides while tracking updates.

Two more consequences close the section:

- The exit is open. The generated Containerfile is a readable build artifact. If the tool stops meeting someone's needs, they take the Containerfile and maintain it by hand. Codegen, not gatekeeping.
- The authoring bar is low. A fragment is a TOML file of flat facts plus files an author already has. A motivated system designer can build one without waiting for a canonical vendor fragment to exist.

### 6. Why this could spread (~300 words)

The one forward-looking section, and the document's close.

Neutrality: everything the model depends on is infrastructure nobody owns. Any registry, any bootc base, existing signers and scanners. A vendor participating does not have to bet on another vendor's toolchain.

First mover: there is no incumbent format for bootc integration knowledge, and no habits to displace. Fragments do not have to convince anyone to abandon a working practice; they can be the form the practice takes from the start.

Bottom-up bootstrap: because the authoring bar is low, consumers can be producers from day one. A vendor arriving later replaces a homegrown fragment with a canonical one, which is an upgrade, not a migration.

The precedent is containers themselves: a neutral mechanism first, an ecosystem after.

## Changes to existing documents

- **docs/rationales.md** keeps its role as the decision record. Add a one-line orientation note at the top pointing to design.md as the design case. Trim only where the explainer now owns an argument wholesale, chiefly the artifact-rather-than-text core of the "Why fragments are standard OCI images" entry, which keeps its alternatives-considered record. All other entries stay.
- **README.md** links design.md as the first document a visitor reaches.

## Non-goals

- No comparison with any named format, tool, or product.
- No claims about unimplemented capability. Every mechanism claim must be verifiable against the current tool.
- No rewrite of rationales.md beyond the trims above.

## Success criteria

- A reader who knows bootc but not this tool can say, after one read: what a fragment is, why it is an OCI image, and what the tool deliberately refuses to do.
- Every behavioral claim checks out against the current codebase, docs/fragment-format.md, and docs/rationales.md.
- The transparency and OCI arguments appear affirmatively, with no named comparisons anywhere in the document.
- The document reads as one argument, not a list of answers.
