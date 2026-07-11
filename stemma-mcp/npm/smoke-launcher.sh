#!/usr/bin/env bash
# End-to-end smoke of the npm distribution: assemble the packages for the host
# platform from a locally built binary, install them the way a consumer would
# (packed tarballs, not symlinks — symlinked installs would not exercise real
# module resolution), then drive the full MCP protocol through npm's bin shim
# with the same smoke test the README documents.
#
#   smoke-launcher.sh <path-to-stemma-mcp-binary>
#
# Requires: node >= 18, npm, python3. Run from anywhere; paths are computed.
set -euo pipefail

binary="${1:?usage: smoke-launcher.sh <path-to-stemma-mcp-binary>}"
binary="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"
[[ -f "$binary" ]] || { echo "error: no binary at $binary" >&2; exit 1; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

case "$(uname -s) $(uname -m)" in
  "Linux x86_64")   triple=x86_64-unknown-linux-gnu   bin_name=stemma-mcp ;;
  "Linux aarch64")  triple=aarch64-unknown-linux-gnu  bin_name=stemma-mcp ;;
  "Darwin x86_64")  triple=x86_64-apple-darwin        bin_name=stemma-mcp ;;
  "Darwin arm64")   triple=aarch64-apple-darwin       bin_name=stemma-mcp ;;
  *) echo "error: unsupported smoke host: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/binaries/$triple"
cp "$binary" "$work/binaries/$triple/$bin_name"

bash "$here/build-npm-packages.sh" --only "$triple" "$work/binaries" "$work/out"

# Pack and install as a consumer project. The platform package installs first
# so the wrapper's registry-pointing optionalDependencies (unpublished during
# development) are never consulted: it is already present, and the wrapper
# itself installs with --omit=optional.
consumer="$work/consumer"
mkdir -p "$consumer"
(cd "$consumer" && npm init -y >/dev/null)
platform_tgz="$(cd "$work/out/mcp-"* && npm pack --silent --pack-destination "$work")"
wrapper_tgz="$(cd "$work/out/mcp" && npm pack --silent --pack-destination "$work")"
(cd "$consumer" && npm install --silent "$work/$(basename "$platform_tgz")")
(cd "$consumer" && npm install --silent --omit=optional "$work/$(basename "$wrapper_tgz")")

shim="$consumer/node_modules/.bin/stemma-mcp"
[[ -x "$shim" ]] || { echo "error: npm did not create the bin shim at $shim" >&2; exit 1; }

echo "--version through the launcher:"
"$shim" --version

echo "full protocol smoke through the launcher:"
python3 "$repo_root/stemma-mcp/smoke_test.py" "$shim" \
  "$repo_root/stemma-examples/samples/safe-agreement.docx"

echo "npm launcher smoke: PASS ($triple)"
