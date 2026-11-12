#!/usr/bin/env bash
# Publish one workspace crate to crates.io without weakening the registry's
# immutable-version contract.
#
#   publish-crate.sh <package>
#
# Steps, all from the committed lockfile and a clean working tree:
#   1. `cargo publish --dry-run` packages and verifies the crate; then
#      `cargo package --no-verify` writes the same bytes to the documented
#      target/package/<name>-<version>.crate, whose SHA-256 is the local
#      integrity (the crates.io index calls it `cksum`).
#   2. The crates.io sparse index is consulted for that exact version:
#      - absent: publish it, then wait until the index lists it with the
#        local checksum;
#      - present with the local checksum: a rerun of an earlier publish, so
#        skip it;
#      - present with any other checksum, or yanked: refuse.
#   Any registry answer other than "listed" or HTTP 404 stops the run.
#
# Package bytes are reproducible for a given commit and cargo version, which
# is what lets a rerun recognise its own earlier upload. Release CI pins the
# toolchain for that reason.
#
# The visibility wait allows STEMMA_CRATES_PUBLISH_POLL_ATTEMPTS x
# STEMMA_CRATES_PUBLISH_POLL_INTERVAL_SECONDS (default 60 x 10 s). It fails
# loudly when it runs out; a rerun resumes through step 2.
#
# Authentication is cargo's: CARGO_REGISTRY_TOKEN (release CI mints a
# short-lived one through crates.io trusted publishing) or `cargo login`.
# This script never reads or prints the token.
set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

[[ "$#" -eq 1 ]] || die "usage: publish-crate.sh <package>"
package="$1"
[[ "$package" =~ ^[a-z][a-z0-9-]*$ ]] || die "invalid package name: $package"

poll_attempts="${STEMMA_CRATES_PUBLISH_POLL_ATTEMPTS:-60}"
poll_interval="${STEMMA_CRATES_PUBLISH_POLL_INTERVAL_SECONDS:-10}"
index_url="${STEMMA_CRATES_INDEX_URL:-https://index.crates.io}"
[[ "$poll_attempts" =~ ^[1-9][0-9]*$ ]] || die "STEMMA_CRATES_PUBLISH_POLL_ATTEMPTS must be a positive integer"
[[ "$poll_interval" =~ ^[0-9]+([.][0-9]+)?$ ]] || die "STEMMA_CRATES_PUBLISH_POLL_INTERVAL_SECONDS must be a non-negative number"

[[ -z "$(git status --porcelain)" ]] || die "working tree is not clean; publish only from the release commit"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if ! cargo metadata --locked --no-deps --format-version 1 >"$work/metadata.json"; then
  die "cargo metadata failed"
fi
if ! identity="$(python3 - "$work/metadata.json" "$package" <<'PY'
import json
import re
import sys

path, package = sys.argv[1:]
with open(path, encoding="utf-8") as stream:
    metadata = json.load(stream)
matches = [entry for entry in metadata.get("packages", []) if entry.get("name") == package]
if len(matches) != 1:
    raise SystemExit("workspace has {} packages named {!r}".format(len(matches), package))
entry = matches[0]
if entry.get("publish") == []:
    raise SystemExit("{} is marked publish = false".format(package))
version = entry.get("version")
if not isinstance(version, str) or re.fullmatch(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version
) is None:
    raise SystemExit("{} must use stable MAJOR.MINOR.PATCH, got {!r}".format(package, version))
target = metadata.get("target_directory")
if not isinstance(target, str) or not target:
    raise SystemExit("cargo metadata has no target_directory")
print(version)
print(target)
PY
)"; then
  die "cannot determine the publishable identity of $package"
fi
version="${identity%%$'\n'*}"
target_dir="${identity#*$'\n'}"
spec="$package@$version"

# Prints "missing", "present <cksum>", or "yanked <cksum>"; any other
# registry state is an error.
query_index() {
  python3 - "$index_url" "$package" "$version" <<'PY'
import json
import sys
import urllib.error
import urllib.request

base, name, version = sys.argv[1:]
lower = name.lower()
if len(lower) <= 2:
    prefix = str(len(lower))
elif len(lower) == 3:
    prefix = "3/" + lower[0]
else:
    prefix = lower[0:2] + "/" + lower[2:4]
url = "{}/{}/{}".format(base.rstrip("/"), prefix, lower)
request = urllib.request.Request(url, headers={"Cache-Control": "no-cache"})
try:
    with urllib.request.urlopen(request, timeout=30) as response:
        body = response.read().decode("utf-8")
except urllib.error.HTTPError as error:
    if error.code == 404:
        print("missing")
        raise SystemExit(0)
    raise SystemExit("crates.io index returned HTTP {} for {}".format(error.code, url))
except (urllib.error.URLError, OSError, UnicodeError) as error:
    raise SystemExit("cannot read crates.io index {}: {}".format(url, error))

entries = []
for number, line in enumerate(body.splitlines(), start=1):
    if not line.strip():
        continue
    try:
        record = json.loads(line)
    except json.JSONDecodeError as error:
        raise SystemExit("malformed index line {} in {}: {}".format(number, url, error))
    if not isinstance(record, dict) or record.get("name", "").lower() != lower:
        raise SystemExit("index line {} in {} is not a {} record".format(number, url, name))
    if record.get("vers") == version:
        entries.append(record)
if not entries:
    print("missing")
    raise SystemExit(0)
if len(entries) > 1:
    raise SystemExit("index lists {}@{} more than once".format(name, version))
record = entries[0]
cksum = record.get("cksum")
if not isinstance(cksum, str) or len(cksum) != 64 or any(c not in "0123456789abcdef" for c in cksum):
    raise SystemExit("index record for {}@{} has no SHA-256 cksum".format(name, version))
yanked = record.get("yanked")
if type(yanked) is not bool:
    raise SystemExit("index record for {}@{} has no boolean yanked flag".format(name, version))
print(("yanked " if yanked else "present ") + cksum)
PY
}

echo "dry run: $spec"
if ! cargo publish --locked -p "$package" --dry-run; then
  die "cargo publish --dry-run failed for $spec"
fi
crate_file="$target_dir/package/$package-$version.crate"
rm -f "$crate_file"
if ! cargo package --locked -p "$package" --no-verify; then
  die "cargo package failed for $spec"
fi
[[ -f "$crate_file" && ! -L "$crate_file" ]] || die "cargo package did not produce $crate_file"
local_cksum="$(sha256sum "$crate_file" | awk '{print $1}')"
[[ "$local_cksum" =~ ^[0-9a-f]{64}$ ]] || die "could not hash $crate_file"
echo "local: $spec cksum $local_cksum"

if ! state="$(query_index)"; then
  die "registry preflight failed for $spec"
fi
case "$state" in
  missing)
    ;;
  "present $local_cksum")
    echo "skip: $spec already published with identical checksum"
    exit 0
    ;;
  present\ *)
    die "refusing existing $spec: local checksum $local_cksum does not match crates.io checksum ${state#present }"
    ;;
  yanked\ *)
    die "refusing $spec: crates.io lists it as yanked (checksum ${state#yanked })"
    ;;
  *)
    die "unexpected registry classification for $spec: $state"
    ;;
esac

echo "publish: $spec"
if ! cargo publish --locked -p "$package"; then
  die "cargo publish failed for $spec"
fi

echo "wait: up to $poll_attempts attempts x ${poll_interval}s for $spec"
for ((attempt = 1; attempt <= poll_attempts; attempt++)); do
  if ! state="$(query_index)"; then
    die "registry verification failed for newly published $spec"
  fi
  case "$state" in
    "present $local_cksum")
      echo "verified: $spec cksum $local_cksum"
      exit 0
      ;;
    missing)
      if (( attempt < poll_attempts )); then
        echo "waiting: $spec not yet in the index (attempt $attempt/$poll_attempts)"
        sleep "$poll_interval"
      fi
      ;;
    present\ * | yanked\ *)
      die "published $spec is listed as '$state'; expected present with checksum $local_cksum"
      ;;
    *)
      die "unexpected registry classification while verifying $spec: $state"
      ;;
  esac
done
die "published $spec was not in the crates.io index after $poll_attempts attempts x ${poll_interval}s. Nothing later was published. Rerun once it is listed: the preflight verifies its checksum and resumes."
