#!/usr/bin/env bash
#
# Fetch the pinned AWS CLI v2 installer zip into hook/ and verify it against a
# recorded sha256.
#
# The zip is ~70 MB and is never committed: it is listed in the repo's
# .gitignore. Run this once before building the fragment image.
#
#   ./fetch-awscli-zip.sh [arch]
#
# arch defaults to this machine's architecture. The fragment image is
# architecture-specific because the zip is; the entrypoint is not.
#
set -euo pipefail

AWSCLI_VERSION="2.36.16"
BASE_URL="https://awscli.amazonaws.com"

FRAGMENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# sha256 of each published zip, captured by downloading both from the official
# location on 2026-08-04. Re-capture on every version bump. An architecture with
# no recorded digest is refused rather than installed unverified.
#
# AWS also publishes a detached PGP signature at <url>.sig, verifiable against
# the public key printed in the AWS CLI installation documentation. The digests
# below are the pin this script enforces; the signature is there if you would
# rather establish the digests yourself than trust this table.
sha256_for_arch() {
    case "$1" in
    x86_64) echo "62a3e23e100ed55715bf3fe804dcec326878a91c05995d7e3843ce0c3f2066f9" ;;
    aarch64) echo "2ca77d39f1e7145ba49ade4c7133aa4474cd24957b2686b87e5307caeb4fff70" ;;
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
    echo "Download awscli-exe-linux-${ARCH}-${AWSCLI_VERSION}.zip, verify it, and add" >&2
    echo "its digest to sha256_for_arch() before building for this architecture." >&2
    exit 1
fi

ZIP_NAME="awscli-exe-linux-${ARCH}-${AWSCLI_VERSION}.zip"
ZIP_PATH="${FRAGMENT_DIR}/hook/${ZIP_NAME}"
ZIP_URL="${BASE_URL}/${ZIP_NAME}"

# macOS ships `shasum`, Linux ships `sha256sum`.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

if [[ -f "$ZIP_PATH" ]] && [[ "$(sha256_of "$ZIP_PATH")" == "$EXPECTED_SHA256" ]]; then
    echo "==> ${ZIP_NAME} already present and verified"
    exit 0
fi

echo "==> downloading ${ZIP_URL}"
curl -fL --retry 3 --retry-delay 2 -o "${ZIP_PATH}.tmp" "$ZIP_URL"

ACTUAL_SHA256="$(sha256_of "${ZIP_PATH}.tmp")"
if [[ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]]; then
    rm -f "${ZIP_PATH}.tmp"
    echo "sha256 mismatch for ${ZIP_NAME}" >&2
    echo "  expected: ${EXPECTED_SHA256}" >&2
    echo "  actual:   ${ACTUAL_SHA256}" >&2
    exit 1
fi
mv "${ZIP_PATH}.tmp" "$ZIP_PATH"

echo "==> verified sha256 ${ACTUAL_SHA256}"
echo "==> ready: hook/${ZIP_NAME} ($(du -h "$ZIP_PATH" | cut -f1))"
