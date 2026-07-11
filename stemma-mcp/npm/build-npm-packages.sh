#!/usr/bin/env bash
# Assemble the npm packages for stemma-mcp from prebuilt binaries.
#
#   build-npm-packages.sh [--only <target-triple>] <binaries-dir> <out-dir>
#
# <binaries-dir> holds one subdirectory per Rust target triple, each containing
# the built binary (stemma-mcp, or stemma-mcp.exe for windows):
#
#   binaries/x86_64-unknown-linux-gnu/stemma-mcp
#   binaries/aarch64-apple-darwin/stemma-mcp
#   ...
#
# Output: <out-dir>/<npm-dir-name>/ — one directory per platform package plus
# the wrapper, each ready for `npm publish`. Versions are stamped from
# stemma-mcp/Cargo.toml (the single source of truth); the wrapper pins its
# optionalDependencies to that exact version.
#
# Default is strict: all five platforms must be present (a release must never
# silently ship a partial matrix). --only <triple> assembles just that platform
# plus the wrapper — for CI smoke tests and local runs.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

only=""
if [[ "${1:-}" == "--only" ]]; then
  only="${2:?--only needs a target triple}"
  shift 2
fi
binaries_dir="${1:?usage: build-npm-packages.sh [--only <triple>] <binaries-dir> <out-dir>}"
out_dir="${2:?usage: build-npm-packages.sh [--only <triple>] <binaries-dir> <out-dir>}"

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/../Cargo.toml" | head -1)"
[[ -n "$version" ]] || { echo "error: could not read version from $here/../Cargo.toml" >&2; exit 1; }

# Keep in sync with PLATFORM_PACKAGES in package/bin/stemma-mcp.js.
# triple : npm package suffix : node os : node cpu : binary name
platforms=(
  "x86_64-unknown-linux-gnu:linux-x64:linux:x64:stemma-mcp"
  "aarch64-unknown-linux-gnu:linux-arm64:linux:arm64:stemma-mcp"
  "x86_64-apple-darwin:darwin-x64:darwin:x64:stemma-mcp"
  "aarch64-apple-darwin:darwin-arm64:darwin:arm64:stemma-mcp"
  "x86_64-pc-windows-msvc:win32-x64:win32:x64:stemma-mcp.exe"
)

mkdir -p "$out_dir"
assembled=0

for entry in "${platforms[@]}"; do
  IFS=':' read -r triple suffix node_os node_cpu bin_name <<<"$entry"
  if [[ -n "$only" && "$triple" != "$only" ]]; then
    continue
  fi
  src="$binaries_dir/$triple/$bin_name"
  if [[ ! -f "$src" ]]; then
    echo "error: missing binary for $triple (expected $src)" >&2
    exit 1
  fi
  pkg_dir="$out_dir/mcp-$suffix"
  mkdir -p "$pkg_dir/bin"
  cp "$src" "$pkg_dir/bin/$bin_name"
  chmod 0755 "$pkg_dir/bin/$bin_name"
  # linux packages declare glibc so musl hosts don't select them.
  libc_line=""
  if [[ "$node_os" == "linux" ]]; then
    libc_line='
  "libc": ["glibc"],'
  fi
  cat >"$pkg_dir/package.json" <<EOF
{
  "name": "@stemma-sh/mcp-$suffix",
  "version": "$version",
  "description": "stemma-mcp prebuilt binary for $node_os $node_cpu. Install @stemma-sh/mcp instead of this package.",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/stemma-sh/stemma.git"
  },
  "license": "(MIT OR Apache-2.0)",
  "preferUnplugged": true,
  "os": ["$node_os"],
  "cpu": ["$node_cpu"],$libc_line
  "files": ["bin"]
}
EOF
  echo "assembled $pkg_dir (from $triple)"
  assembled=$((assembled + 1))
done

if [[ -n "$only" && "$assembled" -eq 0 ]]; then
  echo "error: --only $only matched no known platform triple" >&2
  exit 1
fi

# Wrapper: copy the checked-in package and stamp real versions. With --only,
# the wrapper still pins ALL platforms — npm skips optionalDependencies whose
# os/cpu don't match, and local smoke installs pre-place the platform package.
wrapper_dir="$out_dir/mcp"
rm -rf "$wrapper_dir"
cp -R "$here/package" "$wrapper_dir"
python3 - "$wrapper_dir/package.json" "$version" <<'PY'
import json, sys
path, version = sys.argv[1], sys.argv[2]
with open(path) as f:
    pkg = json.load(f)
pkg["version"] = version
pkg["optionalDependencies"] = {
    name: version for name in pkg["optionalDependencies"]
}
with open(path, "w") as f:
    json.dump(pkg, f, indent=2)
    f.write("\n")
PY
echo "assembled $wrapper_dir (@stemma-sh/mcp $version)"
