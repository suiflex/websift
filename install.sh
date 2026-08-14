#!/bin/sh
# Install the websift binary from a published GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/suiflex/websift/HEAD/install.sh | sh
#
# Environment overrides:
#   VERSION      release tag to install (default: latest)
#   INSTALL_DIR  destination directory (default: $HOME/.local/bin)
set -eu

REPO="suiflex/websift"
BIN="websift"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

fail() {
	echo "install: $1" >&2
	exit 1
}

need() {
	command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

need curl
need tar
need uname
need mktemp

case "$(uname -s)" in
	Darwin) os="apple-darwin" ;;
	Linux) os="unknown-linux-gnu" ;;
	*) fail "unsupported operating system: $(uname -s)" ;;
esac

case "$(uname -m)" in
	x86_64 | amd64) arch="x86_64" ;;
	arm64 | aarch64) arch="aarch64" ;;
	*) fail "unsupported architecture: $(uname -m)" ;;
esac

target="${arch}-${os}"

version="${VERSION:-}"
if [ -z "$version" ]; then
	version=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" |
		sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
		head -n 1)
	[ -n "$version" ] || fail "could not determine the latest release; set VERSION=<tag> and retry"
fi

asset="${BIN}-${version}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${version}"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "install: downloading ${asset}"
curl -fsSL "${base}/${asset}" -o "${tmp}/${asset}" ||
	fail "no release asset for ${target} at ${version}; build from source with: cargo install --git https://github.com/${REPO}"
curl -fsSL "${base}/${asset}.sha256" -o "${tmp}/${asset}.sha256" ||
	fail "checksum file is missing for ${asset}; refusing to install an unverified binary"

# Verify before extracting: a tampered archive must never reach the filesystem.
expected=$(cut -d ' ' -f 1 <"${tmp}/${asset}.sha256")
if command -v sha256sum >/dev/null 2>&1; then
	actual=$(sha256sum "${tmp}/${asset}" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null 2>&1; then
	actual=$(shasum -a 256 "${tmp}/${asset}" | cut -d ' ' -f 1)
else
	fail "no sha256 tool available; install coreutils or perl and retry"
fi
[ -n "$expected" ] || fail "checksum file was empty"
[ "$expected" = "$actual" ] || fail "checksum mismatch for ${asset}; expected ${expected}, got ${actual}"

tar -xzf "${tmp}/${asset}" -C "$tmp"
[ -f "${tmp}/${BIN}" ] || fail "release archive did not contain ${BIN}"

mkdir -p "$INSTALL_DIR"
chmod +x "${tmp}/${BIN}"
mv "${tmp}/${BIN}" "${INSTALL_DIR}/${BIN}"

echo "install: ${BIN} ${version} installed to ${INSTALL_DIR}/${BIN}"

case ":${PATH}:" in
	*":${INSTALL_DIR}:"*) ;;
	*) echo "install: add ${INSTALL_DIR} to PATH, then reopen your shell" ;;
esac

cat <<EOF

Register the server with an agent (nothing else to configure; search works out of the box):

  claude mcp add --scope user websift -- ${INSTALL_DIR}/${BIN} mcp --profile claude-code
  codex mcp add websift -- ${INSTALL_DIR}/${BIN} mcp --profile codex

Check the installation:

  ${INSTALL_DIR}/${BIN} doctor
EOF
