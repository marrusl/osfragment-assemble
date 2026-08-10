#!/usr/bin/env bash
#
# Build and push the example fragments to quay.io/marrusl2/fragments/ as
# linux/amd64+linux/arm64 manifest lists, each tagged with its fragment.toml
# version. Run by hand after `podman login quay.io`; publishing multi-arch is a
# pipeline change with no tool code change. See process-docs/specs/ for the
# multi-arch example fragments design.
#
# Every Containerfile.fragment is FROM scratch with COPY only, so building
# either arch needs no emulation. Emulation matters only at assemble time.
#
set -euo pipefail

REGISTRY="quay.io/marrusl2/fragments"
PLATFORMS="linux/amd64,linux/arm64"
ANNOTATION_NS="com.github.marrusl.osfragment"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FRAGMENTS_DIR="${REPO_ROOT}/examples/fragments"

# Arch-neutral fragments: identical layer bytes across arches, so a single
# multi-platform build produces one shared layer blob that the registry stores
# once. Two separate per-arch builds can emit non-identical gzip and defeat that
# dedup, so these must build in one invocation.
ARCH_NEUTRAL=(
    epel
    tailscale
    grafana
    postgresql
    hashicorp
    cis-hardening
    node-exporter
    nginx
)

# Extract a scalar TOML string value written as `key = "value"`. Returns
# non-zero when the key is absent, which is how vendor (optional) is detected.
toml_scalar() {
    local toml="$1" key="$2" line
    line="$(grep -m1 "^${key} = " "$toml")" || return 1
    line="${line#*\"}"
    line="${line%\"}"
    printf '%s' "$line"
}

# Emit a compact JSON array for a top-level TOML array key, handling both the
# single-line form (`repos = ["a"]`) and the multi-line form postgresql uses.
# An absent key yields `[]`. awk is deliberate: a multi-line TOML array cannot be
# read with shell parameter expansion, and inline `python3 -c` fails silently on
# shell-escaped quotes (see process-docs/skills/registry-verification.md).
toml_json_array() {
    local toml="$1" key="$2"
    awk -v key="$key" '
        $0 ~ "^[[:space:]]*" key "[[:space:]]*=[[:space:]]*\\[" { collecting = 1 }
        collecting {
            buf = buf $0
            if ($0 ~ /\]/) { collecting = 0; done = 1 }
            next
        }
        END {
            if (!done) { print "[]"; exit }
            sub(/^[^[]*\[/, "", buf)
            sub(/\][^]]*$/, "", buf)
            n = 0
            while (match(buf, /"[^"]*"/)) {
                tok[n++] = substr(buf, RSTART, RLENGTH)
                buf = substr(buf, RSTART + RLENGTH)
            }
            out = "["
            for (i = 0; i < n; i++) { out = out tok[i]; if (i < n - 1) out = out "," }
            print out "]"
        }' "$toml"
}

# The tag equals the fragment.toml version, read from the file rather than
# hardcoded so the two never drift.
fragment_tag() {
    local toml="$1" version
    version="$(toml_scalar "$toml" version || true)"
    if [[ -z "$version" ]]; then
        echo "no version found in ${toml}" >&2
        exit 1
    fi
    printf '%s' "$version"
}

# Fill ANN_ARGS with the `--annotation` flags that mirror a fragment's
# fragment.toml. Consumed immediately by the caller. A global rather than a
# return value because bash cannot return an array and the values contain spaces
# (descriptions), so re-splitting a string would corrupt them.
#
# mounts is always set, even to []: the tool's metadata fast path (`list`)
# declines when the mounts annotation is absent, so an empty array is what keeps
# a mount-less fragment on the fast path (src/loader.rs fast_path_from_annotations).
ANN_ARGS=()
build_annotations() {
    local toml="$1"
    local name version description repos required vendor
    name="$(toml_scalar "$toml" name || true)"
    version="$(toml_scalar "$toml" version || true)"
    description="$(toml_scalar "$toml" description || true)"
    if [[ -z "$name" || -z "$version" || -z "$description" ]]; then
        echo "refusing to annotate: missing name/version/description in ${toml}" >&2
        exit 1
    fi
    repos="$(toml_json_array "$toml" repos)"
    required="$(toml_json_array "$toml" required)"
    ANN_ARGS=(
        --annotation "${ANNOTATION_NS}.name=${name}"
        --annotation "${ANNOTATION_NS}.version=${version}"
        --annotation "${ANNOTATION_NS}.description=${description}"
        --annotation "${ANNOTATION_NS}.provides.repos=${repos}"
        --annotation "${ANNOTATION_NS}.packages.required=${required}"
        --annotation "${ANNOTATION_NS}.mounts=[]"
    )
    if vendor="$(toml_scalar "$toml" vendor)" && [[ -n "$vendor" ]]; then
        ANN_ARGS+=(--annotation "${ANNOTATION_NS}.vendor=${vendor}")
    fi
}

# Remove a fragment's per-arch payload from hook/ so the next per-arch build
# context carries only the arch about to be fetched. blob_glob is a glob pattern
# and must stay unquoted to expand.
purge_blob() {
    local dir="$1" blob_glob="$2"
    # shellcheck disable=SC2086  # blob_glob is a glob; unquoted expansion is intended
    rm -f "${dir}/hook/"${blob_glob}
}

# Set the fragment metadata annotations on the image index itself. `list` reads
# annotations via `skopeo inspect --raw`, which for a manifest list returns the
# index, so the fast-path annotations must live on the index and not on the
# per-arch manifests (verified against src/loader.rs fetch_annotations).
annotate_index() {
    local ref="$1" toml="$2"
    build_annotations "$toml"
    podman manifest annotate --index "${ANN_ARGS[@]}" "${ref}"
}

# One single-invocation multi-platform build, then push the whole list.
build_arch_neutral() {
    local name="$1"
    local dir="${FRAGMENTS_DIR}/${name}"
    local tag ref
    tag="$(fragment_tag "${dir}/fragment.toml")"
    ref="${REGISTRY}/${name}:${tag}"

    echo "==> ${name}: building ${PLATFORMS} manifest ${ref}"
    # Drop any stale local list so a re-run does not accumulate duplicate entries.
    podman manifest rm "${ref}" 2>/dev/null || true
    # A stale plain-image tag at the same name defeats --manifest, so clear it too.
    podman rmi -f "${ref}" 2>/dev/null || true
    podman build --platform "${PLATFORMS}" \
        --manifest "${ref}" \
        -f "${dir}/Containerfile.fragment" \
        "${dir}"

    echo "==> ${name}: annotating index"
    annotate_index "${ref}" "${dir}/fragment.toml"

    echo "==> ${name}: pushing ${ref}"
    podman manifest push --all "${ref}"
    echo "==> ${name}: done"
}

# One build context carries one arch's binary, so these cannot use the
# single-invocation form. Fetch each arch's blob in turn, build per arch, then
# assemble the list. blob_glob names the per-arch payload under hook/; it is
# purged before each fetch so a build context never carries the other arch's
# binary (which would otherwise double the published image size).
build_arch_specific() {
    local name="$1" fetch="$2" blob_glob="$3"
    local dir="${FRAGMENTS_DIR}/${name}"
    local tag ref
    tag="$(fragment_tag "${dir}/fragment.toml")"
    ref="${REGISTRY}/${name}:${tag}"

    # Drop any stale per-arch tags so a re-run does not reuse stale per-arch images.
    podman rmi -f "localhost/${name}-amd64" "localhost/${name}-arm64" 2>/dev/null || true

    echo "==> ${name}: building amd64"
    purge_blob "$dir" "$blob_glob"
    "${dir}/${fetch}" x86_64
    podman build --platform linux/amd64 \
        -t "${name}-amd64" \
        -f "${dir}/Containerfile.fragment" \
        "${dir}"

    echo "==> ${name}: building arm64"
    purge_blob "$dir" "$blob_glob"
    "${dir}/${fetch}" aarch64
    podman build --platform linux/arm64 \
        -t "${name}-arm64" \
        -f "${dir}/Containerfile.fragment" \
        "${dir}"

    echo "==> ${name}: assembling manifest ${ref}"
    # Drop any stale local list so a re-run starts from a clean manifest.
    podman manifest rm "${ref}" 2>/dev/null || true
    # A stale plain-image tag at the same name defeats --manifest, so clear it too.
    podman rmi -f "${ref}" 2>/dev/null || true
    podman manifest create "${ref}"
    # podman stores `-t <name>-<arch>` as localhost/<name>-<arch>:latest; the
    # containers-storage transport needs that full name, not the short tag.
    podman manifest add "${ref}" "containers-storage:localhost/${name}-amd64:latest"
    podman manifest add "${ref}" "containers-storage:localhost/${name}-arm64:latest"

    echo "==> ${name}: annotating index"
    annotate_index "${ref}" "${dir}/fragment.toml"

    echo "==> ${name}: pushing ${ref}"
    podman manifest push --all "${ref}"
    echo "==> ${name}: done"
}

for name in "${ARCH_NEUTRAL[@]}"; do
    build_arch_neutral "$name"
done

build_arch_specific awscli-zip fetch-awscli-zip.sh 'awscli-exe-linux-*.zip'
build_arch_specific nvidia-driver-run fetch-run-installer.sh 'NVIDIA-Linux-*.run'

echo "==> all fragments built and pushed"
