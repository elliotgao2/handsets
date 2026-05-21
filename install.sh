#!/usr/bin/env bash
# Handsets installer — macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/elliotgao2/handsets/main/install.sh | bash
#
# Env overrides (all optional):
#   HANDSETS_VERSION  vX.Y.Z   pin a release (default: latest)
#   HANDSETS_DIR      PATH     where to extract (default: $HOME/.handsets)
#   HANDSETS_PREFIX   PATH     where to symlink `hs` (default: first writable
#                                of /usr/local/bin, /opt/homebrew/bin, ~/.local/bin)
#   HANDSETS_REPO     OWNER/N  override repo (default: elliotgao2/handsets)

set -eu

REPO="${HANDSETS_REPO:-elliotgao2/handsets}"
DIR="${HANDSETS_DIR:-$HOME/.handsets}"

say()  { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die()  { warn "handsets: $*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ---------- detect ----------

uname_s=$(uname -s)
uname_m=$(uname -m)

case "$uname_s" in
  Darwin) OS=macos ;;
  Linux)  OS=linux ;;
  MINGW*|MSYS*|CYGWIN*)
    die "Windows detected — use PowerShell: iwr -useb https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex" ;;
  *) die "unsupported OS: $uname_s" ;;
esac

case "$uname_m" in
  arm64|aarch64)
    [ "$OS" = macos ] && ARCH=arm64 || ARCH=aarch64 ;;
  x86_64|amd64) ARCH=x86_64 ;;
  *) die "unsupported arch: $uname_m" ;;
esac

ASSET="handsets-${OS}-${ARCH}.tar.gz"

# ---------- resolve version ----------

VERSION="${HANDSETS_VERSION:-}"
if [ -z "$VERSION" ]; then
  have curl || die "curl is required"
  VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
            | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
  [ -n "$VERSION" ] || die "could not resolve latest release for $REPO"
fi

URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"
SUMS_URL="$URL.sha256"

say "Installing handsets $VERSION ($OS/$ARCH) → $DIR"

# ---------- download ----------

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

curl -fL --progress-bar -o "$TMP/$ASSET"     "$URL"      || die "download failed: $URL"
curl -fsSL              -o "$TMP/$ASSET.sha256" "$SUMS_URL" || warn "no checksum file at $SUMS_URL (skipping verify)"

if [ -s "$TMP/$ASSET.sha256" ]; then
  ( cd "$TMP"
    if have sha256sum; then
      sha256sum -c "$ASSET.sha256" >/dev/null
    elif have shasum; then
      shasum -a 256 -c "$ASSET.sha256" >/dev/null
    else
      warn "no sha256sum/shasum found — skipping verify"
    fi
  ) || die "checksum verification failed"
fi

# ---------- extract ----------

mkdir -p "$DIR"
# Preserve any user state files in $DIR (state-<port>.json etc.); only
# remove the binaries we're about to overwrite.
for f in hs handsets-viewer hs.jar LICENSE VERSION; do
  rm -f "$DIR/$f"
done
tar -xzf "$TMP/$ASSET" -C "$DIR" --strip-components=1
printf '%s\n' "$VERSION" > "$DIR/VERSION"
chmod +x "$DIR/hs" 2>/dev/null || true
[ -f "$DIR/handsets-viewer" ] && chmod +x "$DIR/handsets-viewer" 2>/dev/null || true

# ---------- link into PATH ----------

link_into() {
  ln -sf "$DIR/hs" "$1/hs" 2>/dev/null
}

LINKED=""
if [ -n "${HANDSETS_PREFIX:-}" ]; then
  mkdir -p "$HANDSETS_PREFIX" 2>/dev/null || true
  if link_into "$HANDSETS_PREFIX"; then LINKED="$HANDSETS_PREFIX"; fi
else
  for cand in /usr/local/bin /opt/homebrew/bin "$HOME/.local/bin"; do
    [ -d "$cand" ] || mkdir -p "$cand" 2>/dev/null || continue
    if [ -w "$cand" ] && link_into "$cand"; then
      LINKED="$cand"
      break
    fi
  done
fi

if [ -z "$LINKED" ]; then
  if have sudo; then
    say "Need sudo to symlink into /usr/local/bin …"
    if sudo ln -sf "$DIR/hs" /usr/local/bin/hs; then
      LINKED="/usr/local/bin"
    fi
  fi
fi

# ---------- done ----------

say ""
say "  installed:  $DIR/hs"
[ -f "$DIR/handsets-viewer" ] && say "  viewer:     $DIR/handsets-viewer"
say "  daemon jar: $DIR/hs.jar"

if [ -n "$LINKED" ]; then
  say "  symlinked:  $LINKED/hs"
  case ":$PATH:" in
    *":$LINKED:"*) ;;
    *) say ""; say "Note: $LINKED is not on \$PATH. Add this to your shell rc:"
       say "    export PATH=\"$LINKED:\$PATH\"" ;;
  esac
else
  say ""
  warn "Could not symlink \`hs\` into a bin directory."
  warn "Add this to your shell rc to use it directly:"
  warn "    export PATH=\"$DIR:\$PATH\""
fi

say ""
say "Next:  hs use      # connect a device and start the daemon"
