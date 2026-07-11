#!/usr/bin/env bash
# build-mcpb.sh — package stemma-mcp as an .mcpb bundle for one target.
#
#   ./build-mcpb.sh                 # native host build (fast; needs no zig)
#   ./build-mcpb.sh <alias|triple>  # cross build via cargo-zigbuild (needs zig)
#
# Aliases (desktop targets that Claude Desktop / Claude Code run on):
#   linux,   x86_64-linux            -> x86_64-unknown-linux-gnu
#   linux-arm, aarch64-linux         -> aarch64-unknown-linux-gnu
#   mac, mac-arm, aarch64-darwin     -> aarch64-apple-darwin
#   mac-intel, x86_64-darwin         -> x86_64-apple-darwin
#   windows, x86_64-windows          -> x86_64-pc-windows-gnu
#   <anything else>                  -> used verbatim as a Rust target triple
#
# Output: dist/stemma-<triple>.mcpb
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # stemma-mcp/mcpb
ROOT="$(cd "$HERE/../.." && pwd)"                       # workspace root
BIN="stemma-mcp"
DIST="$HERE/dist"

TARGET_INPUT="${1:-}"

# Build identity: <manifest.json version>+g<sha>[.dirty]. Computed BEFORE the
# build so the binary itself carries it (compile-time STEMMA_MCP_BUILD_STAMP,
# reported in-band in error payloads and open_docx); the same value is written
# into the staged manifest so the host's extension panel agrees with the binary.
BASE_VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$HERE/manifest.json")"
SHA="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$ROOT" diff --quiet HEAD 2>/dev/null || SHA="${SHA}.dirty"
STAMPED="${BASE_VERSION}+g${SHA}"
export STEMMA_MCP_BUILD_STAMP="$STAMPED"

# --- resolve target + build ------------------------------------------------
if [[ -z "$TARGET_INPUT" ]]; then
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
  echo "▸ Native build for host: $TRIPLE"
  ( cd "$ROOT" && cargo build -p "$BIN" --release )
  BUILT="$ROOT/target/release/$BIN"
else
  case "$TARGET_INPUT" in
    linux|x86_64-linux)            TRIPLE="x86_64-unknown-linux-gnu" ;;
    linux-arm|aarch64-linux)       TRIPLE="aarch64-unknown-linux-gnu" ;;
    mac|mac-arm|aarch64-darwin)    TRIPLE="aarch64-apple-darwin" ;;
    mac-intel|x86_64-darwin)       TRIPLE="x86_64-apple-darwin" ;;
    windows|x86_64-windows)        TRIPLE="x86_64-pc-windows-gnu" ;;
    *)                             TRIPLE="$TARGET_INPUT" ;;
  esac
  echo "▸ Cross build for: $TRIPLE (cargo-zigbuild)"
  rustup target add "$TRIPLE" >/dev/null 2>&1 || true
  # uvx fetches cargo-zigbuild on demand; zig must be installed and on PATH.
  ( cd "$ROOT" && uvx cargo-zigbuild zigbuild -p "$BIN" --release --target "$TRIPLE" )
  BUILT="$ROOT/target/$TRIPLE/release/$BIN"
fi

# Windows targets emit a .exe
EXE=""
[[ "$TRIPLE" == *windows* ]] && EXE=".exe"
BUILT="${BUILT}${EXE}"

[[ -f "$BUILT" ]] || { echo "ERROR: built binary not found at $BUILT" >&2; exit 1; }

# --- stage manifest + binary, then pack ------------------------------------
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/server"
cp "$HERE/manifest.json" "$STAGE/manifest.json"
cp "$BUILT" "$STAGE/server/${BIN}${EXE}"
chmod +x "$STAGE/server/${BIN}${EXE}" || true

python3 - "$STAGE/manifest.json" "$STAMPED" <<'PY'
import json, sys
path, version = sys.argv[1], sys.argv[2]
with open(path) as f:
    manifest = json.load(f)
manifest["version"] = version
with open(path, "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY

mkdir -p "$DIST"
OUT="$DIST/stemma-${TRIPLE}.mcpb"
echo "▸ Packing $OUT"
npx --yes @anthropic-ai/mcpb pack "$STAGE" "$OUT"

echo
echo "✅  Built: $OUT  (version $STAMPED)"
echo "   Install: open it in Claude Desktop, or"
echo "            Claude Code Settings → Extensions, or drag-and-drop."
