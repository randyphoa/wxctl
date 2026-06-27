#!/bin/sh
# wxctl installer — Declarative CLI for managing IBM product resources.
#
#   curl -fsSL https://raw.githubusercontent.com/randyphoa/wxctl/main/install.sh | sh
#
# Downloads the latest released binary for your platform, verifies its
# SHA-256 checksum, and installs it to ~/.local/bin. No toolchain required.
#
# Overrides:
#   WXCTL_VERSION       pin a release tag (e.g. v0.1.0); default: latest
#   WXCTL_INSTALL_DIR   install directory;               default: $HOME/.local/bin
# Or pass a version as the first argument:  install.sh v0.1.0

set -eu

REPO="randyphoa/wxctl"
BIN="wxctl"
VERSION="${1:-${WXCTL_VERSION:-}}"
INSTALL_DIR="${WXCTL_INSTALL_DIR:-$HOME/.local/bin}"

info() { printf '\033[34m::\033[0m %s\n' "$*" >&2; }
err()  { printf '\033[31mwxctl install error:\033[0m %s\n' "$*" >&2; exit 1; }

# --- prerequisites ----------------------------------------------------------
if command -v curl >/dev/null 2>&1; then dl() { curl -fsSL -o "$1" "$2"; }
elif command -v wget >/dev/null 2>&1;  then dl() { wget -qO "$1" "$2"; }
else err "need curl or wget"; fi

command -v tar >/dev/null 2>&1 || err "need tar"

# --- detect platform → release target triple --------------------------------
case "$(uname -s)" in
	Darwin) plat="apple-darwin" ;;
	Linux)  plat="unknown-linux-gnu" ;;
	*) err "unsupported OS $(uname -s) — download from https://github.com/${REPO}/releases" ;;
esac
case "$(uname -m)" in
	x86_64|amd64)  cpu="x86_64" ;;
	arm64|aarch64) cpu="aarch64" ;;
	*) err "unsupported architecture $(uname -m)" ;;
esac
TARGET="${cpu}-${plat}"

# --- resolve version --------------------------------------------------------
if [ -z "$VERSION" ]; then
	api="https://api.github.com/repos/${REPO}/releases/latest"
	VERSION="$(dl /dev/stdout "$api" | grep '"tag_name"' | head -1 | sed -E 's/.*"([^"]+)".*/\1/')"
	[ -n "$VERSION" ] || err "could not resolve latest release — pin one: install.sh v0.1.0"
fi

ARCHIVE="${BIN}-${VERSION}-${TARGET}.tar.gz"
BASE="https://github.com/${REPO}/releases/download/${VERSION}"
info "installing ${BIN} ${VERSION} (${TARGET})"

# --- download + verify + extract --------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

dl "$TMP/$ARCHIVE" "${BASE}/${ARCHIVE}" || err "download failed — no ${TARGET} build for ${VERSION}?"

if dl "$TMP/SHA256SUMS" "${BASE}/SHA256SUMS" 2>/dev/null; then
	if command -v sha256sum >/dev/null 2>&1; then sum() { sha256sum "$1" | awk '{print $1}'; }
	elif command -v shasum >/dev/null 2>&1;  then sum() { shasum -a 256 "$1" | awk '{print $1}'; }
	else sum() { return 1; }; fi
	want="$(grep " ${ARCHIVE}\$" "$TMP/SHA256SUMS" | awk '{print $1}')"
	if got="$(sum "$TMP/$ARCHIVE")" && [ -n "$want" ]; then
		[ "$got" = "$want" ] || err "checksum mismatch — refusing to install"
		info "checksum verified"
	fi
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
src="$TMP/${BIN}-${VERSION}-${TARGET}/${BIN}"
[ -f "$src" ] || src="$(find "$TMP" -type f -name "$BIN" | head -1)"
[ -f "$src" ] || err "binary not found inside ${ARCHIVE}"

# --- install ----------------------------------------------------------------
mkdir -p "$INSTALL_DIR" || err "cannot create $INSTALL_DIR"
if ! { cp "$src" "$INSTALL_DIR/$BIN" && chmod +x "$INSTALL_DIR/$BIN"; }; then
	err "cannot write to $INSTALL_DIR — set WXCTL_INSTALL_DIR to a writable path"
fi

info "installed → $INSTALL_DIR/$BIN"

case ":${PATH}:" in
	*":$INSTALL_DIR:"*) ;;
	*) info "add to your shell profile:  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

info "done — run '${BIN} --help' to get started"
