#!/usr/bin/env bash
# build-plugin.sh — package stemma-mcp as a Claude plugin zip.
#
# The bundle is a self-contained Claude plugin: the stemma-mcp server binary
# and the .mcp.json that wires it up. Canonical agent guidance is served by the
# MCP server through initialize instructions and tool descriptions. A plugin connector
# runs ON THE HOST machine and shares its filesystem with the agent, so (unlike
# a remote-style .mcpb desktop extension) the agent's own file paths work with
# open_docx. The bundled binary must match the host you run on.
#
#   ./build-plugin.sh                 # native host build (fast; no zig)
#   ./build-plugin.sh <alias|triple>  # cross build via cargo-zigbuild (needs zig)
#
# Aliases: mac, mac-arm, aarch64-darwin -> aarch64-apple-darwin
#          mac-intel, x86_64-darwin     -> x86_64-apple-darwin
#          linux, x86_64-linux          -> x86_64-unknown-linux-gnu
#          linux-arm, aarch64-linux     -> aarch64-unknown-linux-gnu
#          <anything else>              -> used verbatim as a Rust target triple
#
# Output: dist/stemma-plugin.zip  (install as a Claude plugin in your client's
#         plugin settings)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # stemma-mcp/plugin
ROOT="$(cd "$HERE/../.." && pwd)"                       # workspace root
BIN="stemma-mcp"
DIST="$HERE/dist"

TARGET_INPUT="${1:-}"

# Build identity: <plugin.json version>+g<sha>[.dirty]. Computed BEFORE the
# build so the binary itself carries it (compile-time STEMMA_MCP_BUILD_STAMP,
# reported in-band in error payloads and open_docx); the same value is written
# into the staged manifest so the host's plugin panel agrees with the binary.
BASE_VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$HERE/.claude-plugin/plugin.json")"
SHA="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
git -C "$ROOT" diff --quiet HEAD 2>/dev/null || SHA="${SHA}.dirty"
STAMPED="${BASE_VERSION}+g${SHA}"
export STEMMA_MCP_BUILD_STAMP="$STAMPED"

if [[ -z "$TARGET_INPUT" ]]; then
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
  echo "▸ Native build for host: $TRIPLE"
  ( cd "$ROOT" && cargo build -p "$BIN" --release )
  BUILT="$ROOT/target/release/$BIN"
else
  case "$TARGET_INPUT" in
    mac|mac-arm|aarch64-darwin)   TRIPLE="aarch64-apple-darwin" ;;
    mac-intel|x86_64-darwin)      TRIPLE="x86_64-apple-darwin" ;;
    linux|x86_64-linux)           TRIPLE="x86_64-unknown-linux-gnu" ;;
    linux-arm|aarch64-linux)      TRIPLE="aarch64-unknown-linux-gnu" ;;
    *)                            TRIPLE="$TARGET_INPUT" ;;
  esac
  echo "▸ Cross build for: $TRIPLE (cargo-zigbuild)"
  rustup target add "$TRIPLE" >/dev/null 2>&1 || true
  ( cd "$ROOT" && uvx cargo-zigbuild zigbuild -p "$BIN" --release --target "$TRIPLE" )
  BUILT="$ROOT/target/$TRIPLE/release/$BIN"
fi

[[ -f "$BUILT" ]] || { echo "ERROR: built binary not found at $BUILT" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/.claude-plugin" "$STAGE/server"
cp "$HERE/.claude-plugin/plugin.json" "$STAGE/.claude-plugin/plugin.json"
cp "$HERE/.mcp.json"                   "$STAGE/.mcp.json"
cp "$BUILT"                            "$STAGE/server/$BIN"
chmod +x "$STAGE/server/$BIN"
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$STAGE/"

python3 - "$STAGE/.claude-plugin/plugin.json" "$STAMPED" <<'PY'
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
OUT="$DIST/stemma-plugin.zip"
rm -f "$OUT"
if command -v zip >/dev/null 2>&1; then
  ( cd "$STAGE" && zip -qr "$OUT" . )
else
  # Fallback when the `zip` CLI is absent: python3 preserves the executable bit
  # on server/stemma-mcp (without it, the extracted binary won't spawn).
  ( cd "$STAGE" && python3 - "$OUT" <<'PY'
import os, sys, zipfile
out = sys.argv[1]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for root, _, files in os.walk("."):
        for f in files:
            p = os.path.join(root, f)
            zi = zipfile.ZipInfo(os.path.relpath(p, "."))
            zi.external_attr = (os.stat(p).st_mode & 0xFFFF) << 16
            zi.compress_type = zipfile.ZIP_DEFLATED
            with open(p, "rb") as fh:
                z.writestr(zi, fh.read())
PY
  )
fi

echo
echo "✅  Built: $OUT  (version $STAMPED, target: $TRIPLE)"
echo "   Install: add this .zip as a Claude plugin in your client's plugin settings."
