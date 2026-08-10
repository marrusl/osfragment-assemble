#!/usr/bin/env bash
#
# Fetch the pinned NVIDIA .run installer into hook/, verify it against a
# recorded sha256, and extract the LICENSE that has to travel with it.
#
# The installer is ~300 MB and is never committed: it is listed in the repo's
# .gitignore, along with the two LICENSE copies this script derives from it.
# Run this once before building the fragment image.
#
#   ./fetch-run-installer.sh [arch]
#
# arch defaults to this machine's architecture. The fragment image is
# architecture-specific because the installer is.
#
set -euo pipefail

DRIVER_VERSION="610.57.04"
BASE_URL="https://download.nvidia.com/XFree86"

FRAGMENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LICENSE_IN_IMAGE="${FRAGMENT_DIR}/tree/usr/share/licenses/nvidia-driver-run/LICENSE"
LICENSE_WITH_BLOB="${FRAGMENT_DIR}/hook/LICENSE"

# sha256 of each published installer, captured by downloading each from NVIDIA's
# official location (aarch64 on 2026-08-04, x86_64 on 2026-08-09). Re-capture on
# every version bump. An architecture with no recorded digest is refused rather
# than installed unverified.
sha256_for_arch() {
    case "$1" in
    x86_64) echo "b2e935c66b83bb00c0c857bc8e0ee0fd52de9286b40c9cc1eec29a7ce7eb116d" ;;
    aarch64) echo "40279facc0429a93b0b8ec97bf59391a3d2207609894f8271b7253a14c3f8f9e" ;;
    *) return 1 ;;
    esac
}

case "${1:-$(uname -m)}" in
arm64 | aarch64) ARCH="aarch64" ;;
amd64 | x86_64) ARCH="x86_64" ;;
*) echo "unsupported architecture: ${1:-$(uname -m)}" >&2 && exit 1 ;;
esac

if ! EXPECTED_SHA256="$(sha256_for_arch "$ARCH")"; then
    echo "no sha256 recorded for ${ARCH}." >&2
    echo "Download NVIDIA-Linux-${ARCH}-${DRIVER_VERSION}.run, verify it, and add" >&2
    echo "its digest to sha256_for_arch() before building for this architecture." >&2
    exit 1
fi

RUN_NAME="NVIDIA-Linux-${ARCH}-${DRIVER_VERSION}.run"
RUN_PATH="${FRAGMENT_DIR}/hook/${RUN_NAME}"
RUN_URL="${BASE_URL}/Linux-${ARCH}/${DRIVER_VERSION}/${RUN_NAME}"

# macOS ships `shasum`, Linux ships `sha256sum`.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

if [[ -f "$RUN_PATH" ]] && [[ "$(sha256_of "$RUN_PATH")" == "$EXPECTED_SHA256" ]]; then
    echo "==> ${RUN_NAME} already present and verified"
else
    echo "==> downloading ${RUN_URL}"
    curl -fL --retry 3 --retry-delay 2 -o "${RUN_PATH}.tmp" "$RUN_URL"

    ACTUAL_SHA256="$(sha256_of "${RUN_PATH}.tmp")"
    if [[ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]]; then
        rm -f "${RUN_PATH}.tmp"
        echo "sha256 mismatch for ${RUN_NAME}" >&2
        echo "  expected: ${EXPECTED_SHA256}" >&2
        echo "  actual:   ${ACTUAL_SHA256}" >&2
        exit 1
    fi
    mv "${RUN_PATH}.tmp" "$RUN_PATH"
    echo "==> verified sha256 ${ACTUAL_SHA256}"
fi

# NVIDIA's distribution grant is conditional on the agreement reaching each
# recipient, so the LICENSE is taken from the verified archive itself rather
# than committed separately, and shipped at both hops: hook/LICENSE travels
# with the blob for recipients of the FRAGMENT, and the tree/ copy lands in
# /usr/share/licenses for recipients of the built OS IMAGE.
echo "==> extracting LICENSE from ${RUN_NAME}"
EXTRACT_DIR="$(mktemp -d)"
trap 'rm -rf "$EXTRACT_DIR"' EXIT

( cd "$EXTRACT_DIR" && sh "$RUN_PATH" --extract-only >/dev/null )

EXTRACTED_LICENSE="${EXTRACT_DIR}/NVIDIA-Linux-${ARCH}-${DRIVER_VERSION}/LICENSE"
if [[ ! -f "$EXTRACTED_LICENSE" ]]; then
    echo "LICENSE not found in the extracted installer" >&2
    exit 1
fi

mkdir -p "$(dirname "$LICENSE_IN_IMAGE")"
cp "$EXTRACTED_LICENSE" "$LICENSE_IN_IMAGE"
cp "$EXTRACTED_LICENSE" "$LICENSE_WITH_BLOB"

echo "==> ready: hook/${RUN_NAME} ($(du -h "$RUN_PATH" | cut -f1))"
echo "==> ready: hook/LICENSE and tree/usr/share/licenses/nvidia-driver-run/LICENSE"
