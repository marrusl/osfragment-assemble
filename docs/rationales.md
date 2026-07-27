# Design Rationales

Engineering decisions behind osfragment-assemble's fragment format and assembly model.

## Why fragments are standard OCI images

Fragments use the existing OCI distribution stack — no new builder, no custom registry plugins, no client-side tooling changes. Vendors can publish fragments to quay.io, GHCR, or ECR using the same workflows they use for container images. Customers can pull them with skopeo, mirror them with podman, and scan them with existing supply chain tools.

Alternative considered: a custom archive format (`.tar.gz` or `.zip`). Rejected because it requires a separate distribution story and doesn't benefit from existing registry infrastructure.

## Why no fragment type taxonomy

All fragments follow the same format (`fragment.toml`, `tree/`, `scripts/`). The `phase` field controls ordering, but there's no type system distinguishing "repo fragments" from "config fragments" from "service fragments". A fragment that installs a repo is structurally identical to one that drops a config file.

This keeps the format simple and avoids artificial constraints — a fragment can deliver repo definitions, config files, and scripts in a single unit when that's the right packaging boundary.

## Why the tool generates Containerfiles instead of building directly

The generated Containerfile is a build artifact customers can read, edit, and version. No lock-in. If osfragment-assemble stops meeting their needs, they take the Containerfile and maintain it manually. The tool's job is codegen, not gatekeeping.

Building directly would make the tool a required dependency in the build pipeline and hide the actual image construction steps from operators.

## Why manifest-declared packages are preferred over script-installed packages

Packages declared in the manifest's `packages:` field are batched into a single `dnf install` layer and deduplicated across fragments. This gives the tool visibility into what's being installed and keeps the generated Containerfile predictable.

Scripts that call `dnf` or run vendor installers (like NVIDIA's `.run` binaries) are not prevented — the tool doesn't enforce this. But packages installed by scripts bypass deduplication, won't appear in the manifest's package list, and create additional layers. When possible, prefer the manifest; when a script genuinely needs to install packages (vendor installers, complex dependency chains), that's fine.

## Why digest pinning is opt-in

Digest pinning guarantees reproducibility but makes the generated Containerfile harder to read and breaks registry mirrors that don't preserve digests. The default (`--pin-digests` omitted) uses tags, which are more human-friendly and work with disconnected/airgap scenarios where images are re-pushed to internal registries.

When digests are pinned, the tool switches to named stages (`AS frag-<name>`) for readability — otherwise it uses inline `COPY --from=<image-ref>` to keep the Containerfile compact.

## Why the tool cleans up dnf artifacts in the same RUN layer

The `RUN dnf install ... && dnf clean all && rm -rf /var/log/dnf*` pattern runs in a single layer to prevent dnf metadata from inflating the image size. If cleanup ran in a separate `RUN`, the previous layer would still carry the full dnf cache even though it's deleted in the next layer.

This is standard Containerfile practice, not specific to osfragment-assemble.

## Why `FROM configs AS final` for OCP mode

OpenShift's on-cluster build system uses `FROM configs AS final` to mark the stage MCO should extract. The base image and fragment stages are build-time dependencies only — the `final` stage is what gets deployed to nodes.

Standalone mode uses `FROM <base-image>` because there's no special stage marker needed outside the MCO context.

## Why inline image refs by default, named stages when pinning

Unpinned: `COPY --from=quay.io/example/fragment:1.0 /fragment/tree/ /`  
Pinned: `FROM quay.io/example/fragment@sha256:... AS frag-example` then `COPY --from=frag-example /fragment/tree/ /`

Digests are long and unreadable. Named stages make pinned Containerfiles easier to review. Unpinned refs are short enough to inline without harming readability.

## Why config files land after packages

Fragment config files are copied to the target image after package installation completes. This guarantees that fragment configurations always win when they overlap with RPM-installed files.

During RPM installation, if a package installs a file that already exists on disk and isn't marked as `%config` in the RPM spec, the RPM will overwrite the existing file. If fragment configs were copied before packages, RPM installs could silently replace them with package defaults. Copying configs after package installation ensures fragment-supplied configurations are never overwritten — the intended state from the fragment is what lands in the final image.

## Trust boundary

Fragments are trusted build code, not passive data. A fragment's `configure.sh` runs as root during image assembly. Pulling a fragment is equivalent to running an upstream install script — it's a supply chain trust decision. The tool does not sandbox fragment scripts or validate their behavior.

Repo deduplication prevents different fragments from silently overwriting each other's repos, but it's not a security boundary — if two fragments provide the same repo with different content, the build fails rather than choosing one.
