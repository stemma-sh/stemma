#!/usr/bin/env bash
# Package approved release binaries into deterministic per-target archives.
#
# Usage: package_release_assets.sh <binaries-dir> <version> <output-dir>
#
# <binaries-dir> is a verified candidate download: one directory per target
# plus `candidate-manifest`. Archives are byte-reproducible from the same
# inputs (fixed 1980 timestamps, sorted entries, no gzip name/mtime header),
# so a retry produces assets identical to the ones already uploaded.
#
# This lives in a script rather than inline in a workflow on purpose: a
# heredoc inside a YAML block scalar cannot be indented to match surrounding
# shell structure without silently breaking, which is exactly how the 0.2.0
# release lost its assets.

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <binaries-dir> <version> <output-dir>" >&2
  exit 2
fi

binaries="$1"
version="$2"
output="$3"

if [ ! -d "$binaries" ]; then
  echo "error: binaries directory does not exist: $binaries" >&2
  exit 1
fi
if [ ! -f "$binaries/candidate-manifest/candidate-manifest.json" ]; then
  echo "error: $binaries is not a verified candidate download (no manifest)" >&2
  exit 1
fi

mkdir -p "$output"
cp "$binaries/candidate-manifest/candidate-manifest.json" "$output/"

packaged=0
for dir in "$binaries"/*/; do
  target="$(basename "$dir")"
  if [ "$target" = "candidate-manifest" ]; then
    continue
  fi
  case "$target" in
    *windows*)
      python3 - "$dir" "$output/stemma-mcp-$version-$target.zip" <<'PY'
import pathlib
import sys
import zipfile

source = pathlib.Path(sys.argv[1])
output = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(output, "x", zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(source.iterdir(), key=lambda item: item.name):
        info = zipfile.ZipInfo(path.name, (1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o644 << 16
        archive.writestr(info, path.read_bytes())
PY
      ;;
    *)
      chmod 0755 "$dir"/stemma-mcp
      tar --sort=name --mtime='UTC 1980-01-01' \
        --owner=0 --group=0 --numeric-owner --format=ustar \
        -C "$dir" -cf - . \
        | gzip -n > "$output/stemma-mcp-$version-$target.tar.gz"
      ;;
  esac
  packaged=$((packaged + 1))
done

if [ "$packaged" -eq 0 ]; then
  echo "error: no target directories found under $binaries" >&2
  exit 1
fi

# Expansion happens before the redirect is created, so sha256sums.txt does
# not list itself.
(cd "$output" && sha256sum * > sha256sums.txt)
echo "packaged $packaged targets into $output"
