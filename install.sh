#!/bin/sh
set -eu

# Install dv.
#
#   curl -fsSL https://raw.githubusercontent.com/adrum/dv/main/install.sh | sh
#
# Downloads the newest published dv for this platform, verifies it against the
# sha256 published beside it, and puts the binary on disk. Nothing else — it
# does not edit shell profiles, because a script that rewrites your rc files
# from a pipe is a worse trade than a line you paste yourself.
#
# Deliberately POSIX sh with no JSON parsing: this is the one piece of dv that
# runs before dv exists, on a machine we know nothing about. Everything it
# needs is in the asset names and a .sha256 sidecar.

REPO="${DV_REPO:-adrum/dv}"
BASE_URL="${DV_INSTALL_BASE_URL:-https://github.com/${REPO}/releases/latest/download}"
BIN_DIR="${DV_BIN_DIR:-${HOME}/.local/bin}"

die() { echo "dv install: $*" >&2; exit 1; }

# https is pinned for the published location. An explicit DV_INSTALL_BASE_URL
# is someone deliberately pointing at a mirror or a test server, so the pin
# follows the scheme they chose rather than blocking them — but it says so.
case "${BASE_URL}" in
    https://*) proto_args="--proto =https --tlsv1.2" ;;
    *)
        proto_args=""
        echo "dv install: warning: ${BASE_URL} is not https" >&2
        ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }
need curl
need tar

# Asset names use .NET runtime identifiers, the same names dv itself uses for
# SDK downloads.
case "$(uname -s)" in
    Darwin) os="osx" ;;
    Linux)
        os="linux"
        # Alpine and friends: the static build runs there too, but it is
        # published under the name dv would look for on that machine.
        if ls /lib/ld-musl-* >/dev/null 2>&1; then os="linux-musl"; fi
        ;;
    *) die "unsupported operating system $(uname -s) — on Windows, download dv-win-x64.tar.gz from the releases page" ;;
esac
case "$(uname -m)" in
    arm64|aarch64) arch="arm64" ;;
    x86_64|amd64)  arch="x64" ;;
    *) die "unsupported architecture $(uname -m)" ;;
esac
platform="${os}-${arch}"

archive="dv-${platform}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT INT TERM

echo "dv install: fetching ${archive}"
# shellcheck disable=SC2086  # proto_args is intentionally word-split
curl -fsSL ${proto_args} -o "${tmp}/${archive}" "${BASE_URL}/${archive}" ||
    die "could not download ${BASE_URL}/${archive}"
# shellcheck disable=SC2086  # proto_args is intentionally word-split
curl -fsSL ${proto_args} -o "${tmp}/${archive}.sha256" "${BASE_URL}/${archive}.sha256" ||
    die "could not download the checksum for ${archive} — refusing to install unverified"

# Verification fails closed: no checksum, a malformed one, or a mismatch all
# stop the install. An unverifiable download is not a reason to continue.
expected="$(awk '{print $1; exit}' "${tmp}/${archive}.sha256")"
case "${expected}" in
    ????????????????????????????????????????????????????????????????) ;;
    *) die "the published checksum for ${archive} is not a sha256 — refusing to install" ;;
esac

if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${tmp}/${archive}" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${tmp}/${archive}" | awk '{print $1}')"
else
    die "neither shasum nor sha256sum is available — cannot verify the download"
fi
[ "${expected}" = "${actual}" ] ||
    die "checksum mismatch for ${archive} (expected ${expected}, got ${actual}) — nothing was installed"

tar -xzf "${tmp}/${archive}" -C "${tmp}"
binary="$(find "${tmp}" -type f -name dv -perm -u+x -print | head -n 1)"
[ -n "${binary}" ] || die "${archive} did not contain a dv binary"

mkdir -p "${BIN_DIR}" || die "could not create ${BIN_DIR}"
# Install by rename so a half-copied binary is never left on PATH, and so
# replacing a running dv is safe.
cp "${binary}" "${BIN_DIR}/dv.incoming" || die "could not write to ${BIN_DIR}"
chmod 0755 "${BIN_DIR}/dv.incoming"
mv "${BIN_DIR}/dv.incoming" "${BIN_DIR}/dv"

echo "dv install: installed $("${BIN_DIR}/dv" --version) to ${BIN_DIR}/dv"

case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *)
        echo
        echo "${BIN_DIR} is not on your PATH. Add it:"
        echo "  export PATH=\"${BIN_DIR}:\$PATH\""
        ;;
esac

echo
echo "Next:"
echo "  dv install lts          # get an SDK"
echo "  eval \"\$(dv init zsh)\"   # add this to your shell profile"
echo "  dv doctor               # confirm it worked"
