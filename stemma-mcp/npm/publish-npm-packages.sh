#!/usr/bin/env bash
# Publish a fully assembled npm package matrix without weakening npm's
# immutable-version contract. Platform packages are always published before
# the wrapper that references them.
#
#   publish-npm-packages.sh <assembled-packages-dir>
#
# The input must contain exactly the six directories emitted by
# build-npm-packages.sh. Every local package and every remote version is
# reconciled before the first publish. Only a structured registry E404 means
# that a version is missing; every other command or JSON failure stops the run.
set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

[[ "$#" -eq 1 ]] || die "usage: publish-npm-packages.sh <assembled-packages-dir>"
assembled_root="$1"
[[ -d "$assembled_root" ]] || die "assembled package directory does not exist: $assembled_root"
assembled_root="$(cd "$assembled_root" && pwd)"

poll_attempts="${STEMMA_NPM_PUBLISH_POLL_ATTEMPTS:-30}"
poll_interval="${STEMMA_NPM_PUBLISH_POLL_INTERVAL_SECONDS:-2}"
[[ "$poll_attempts" =~ ^[1-9][0-9]*$ ]] || die "STEMMA_NPM_PUBLISH_POLL_ATTEMPTS must be a positive integer"
[[ "$poll_interval" =~ ^[0-9]+([.][0-9]+)?$ ]] || die "STEMMA_NPM_PUBLISH_POLL_INTERVAL_SECONDS must be a non-negative number"

# Keep this explicit order in sync with build-npm-packages.sh. Globs are not
# acceptable here: the wrapper must be last, and an unexpected directory must
# never become publishable merely because its name matches a pattern.
package_dirs=(
  "mcp-linux-x64"
  "mcp-linux-arm64"
  "mcp-darwin-x64"
  "mcp-darwin-arm64"
  "mcp-win32-x64"
  "mcp"
)
package_names=(
  "@stemma-sh/mcp-linux-x64"
  "@stemma-sh/mcp-linux-arm64"
  "@stemma-sh/mcp-darwin-x64"
  "@stemma-sh/mcp-darwin-arm64"
  "@stemma-sh/mcp-win32-x64"
  "@stemma-sh/mcp"
)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 - "$assembled_root" "${package_dirs[@]}" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
expected = set(sys.argv[2:])
actual = {entry.name for entry in root.iterdir()}
missing = sorted(expected - actual)
extra = sorted(actual - expected)
if missing:
    raise SystemExit("missing assembled package directories: {}".format(", ".join(missing)))
if extra:
    raise SystemExit("unexpected assembled package entries: {}".format(", ".join(extra)))
for name in sorted(expected):
    package_dir = root / name
    manifest = package_dir / "package.json"
    if package_dir.is_symlink() or not package_dir.is_dir():
        raise SystemExit("assembled package must be a real directory: {}".format(package_dir))
    if manifest.is_symlink() or not manifest.is_file():
        raise SystemExit("missing regular package manifest: {}".format(manifest))
PY

read_package_version() {
  local manifest="$1"
  local expected_name="$2"
  python3 - "$manifest" "$expected_name" <<'PY'
import json
import re
import sys

path, expected_name = sys.argv[1:]

def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result

try:
    with open(path, encoding="utf-8") as stream:
        package = json.load(stream, object_pairs_hook=unique_object)
except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
    raise SystemExit("cannot parse {}: {}".format(path, error))
if not isinstance(package, dict):
    raise SystemExit("{} must contain a JSON object".format(path))
if package.get("name") != expected_name:
    raise SystemExit(
        "{} has package name {!r}; expected {!r}".format(
            path, package.get("name"), expected_name
        )
    )
scripts = package.get("scripts")
if scripts not in (None, {}):
    raise SystemExit(
        "{} must not declare lifecycle scripts for immutable publication".format(path)
    )
version = package.get("version")
semver = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)
if not isinstance(version, str) or semver.fullmatch(version) is None:
    raise SystemExit(
        "{} must use stable MAJOR.MINOR.PATCH SemVer, got {!r}".format(path, version)
    )
print(version)
PY
}

versions=()
common_version=""
for index in "${!package_dirs[@]}"; do
  manifest="$assembled_root/${package_dirs[$index]}/package.json"
  version="$(read_package_version "$manifest" "${package_names[$index]}")"
  versions[$index]="$version"
  if [[ -z "$common_version" ]]; then
    common_version="$version"
  elif [[ "$version" != "$common_version" ]]; then
    die "assembled package versions disagree: ${package_names[$index]} is $version, expected $common_version"
  fi
done

python3 - "$assembled_root/mcp/package.json" "$common_version" "${package_names[@]:0:5}" <<'PY'
import json
import sys

path, version, *platform_names = sys.argv[1:]

def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result

try:
    with open(path, encoding="utf-8") as stream:
        package = json.load(stream, object_pairs_hook=unique_object)
except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
    raise SystemExit("cannot parse wrapper manifest {}: {}".format(path, error))
expected = {name: version for name in platform_names}
actual = package.get("optionalDependencies")
if actual != expected:
    raise SystemExit(
        "wrapper optionalDependencies do not exactly pin the assembled platform matrix"
    )
PY

parse_pack_result() {
  local json_path="$1"
  local expected_name="$2"
  local expected_version="$3"
  python3 - "$json_path" "$expected_name" "$expected_version" <<'PY'
import base64
import binascii
import json
import re
import sys

path, expected_name, expected_version = sys.argv[1:]

def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result

try:
    with open(path, encoding="utf-8") as stream:
        payload = json.load(stream, object_pairs_hook=unique_object)
except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
    raise SystemExit("malformed npm pack JSON: {}".format(error))
if not isinstance(payload, list) or len(payload) != 1 or not isinstance(payload[0], dict):
    raise SystemExit("npm pack JSON must be a one-element object array")
entry = payload[0]
if entry.get("name") != expected_name or entry.get("version") != expected_version:
    raise SystemExit(
        "npm pack identity mismatch: got {!r}@{!r}, expected {}@{}".format(
            entry.get("name"), entry.get("version"), expected_name, expected_version
        )
    )
integrity = entry.get("integrity")
if not isinstance(integrity, str) or not integrity.startswith("sha512-"):
    raise SystemExit("npm pack did not return a sha512 integrity")
try:
    digest = base64.b64decode(integrity[len("sha512-"):], validate=True)
except (ValueError, binascii.Error) as error:
    raise SystemExit("npm pack returned invalid sha512 integrity: {}".format(error))
if len(digest) != 64:
    raise SystemExit("npm pack returned a sha512 integrity with the wrong digest length")
filename = entry.get("filename")
if not isinstance(filename, str) or re.fullmatch(r"[0-9A-Za-z._+-]+[.]tgz", filename) is None:
    raise SystemExit("npm pack returned an unsafe tarball filename {!r}".format(filename))
print(integrity)
print(filename)
PY
}

file_integrity() {
  local tarball="$1"
  python3 - "$tarball" <<'PY'
import base64
import hashlib
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
if path.is_symlink() or not path.is_file():
    raise SystemExit("packed tarball is not a regular file: {}".format(path))
digest = hashlib.sha512()
with path.open("rb") as stream:
    while True:
        chunk = stream.read(1024 * 1024)
        if not chunk:
            break
        digest.update(chunk)
print("sha512-" + base64.b64encode(digest.digest()).decode("ascii"))
PY
}

local_integrities=()
tarballs=()
for index in "${!package_dirs[@]}"; do
  package_dir="$assembled_root/${package_dirs[$index]}"
  dry_stdout="$work/pack-dry-$index.json"
  dry_stderr="$work/pack-dry-$index.stderr"
  if ! npm pack --dry-run --json --ignore-scripts "$package_dir" >"$dry_stdout" 2>"$dry_stderr"; then
    [[ ! -s "$dry_stderr" ]] || sed 's/^/npm pack: /' "$dry_stderr" >&2
    die "npm pack --dry-run failed for ${package_names[$index]}@${versions[$index]}"
  fi
  if ! dry_result="$(parse_pack_result "$dry_stdout" "${package_names[$index]}" "${versions[$index]}")"; then
    die "could not establish local integrity for ${package_names[$index]}@${versions[$index]}"
  fi
  dry_integrity="${dry_result%%$'\n'*}"
  dry_filename="${dry_result#*$'\n'}"

  tarball_dir="$work/tarballs/$index"
  mkdir -p "$tarball_dir"
  pack_stdout="$work/pack-real-$index.json"
  pack_stderr="$work/pack-real-$index.stderr"
  if ! npm pack --json --ignore-scripts --pack-destination "$tarball_dir" "$package_dir" >"$pack_stdout" 2>"$pack_stderr"; then
    [[ ! -s "$pack_stderr" ]] || sed 's/^/npm pack: /' "$pack_stderr" >&2
    die "npm pack failed for ${package_names[$index]}@${versions[$index]}"
  fi
  if ! pack_result="$(parse_pack_result "$pack_stdout" "${package_names[$index]}" "${versions[$index]}")"; then
    die "could not establish packed integrity for ${package_names[$index]}@${versions[$index]}"
  fi
  pack_integrity="${pack_result%%$'\n'*}"
  pack_filename="${pack_result#*$'\n'}"
  [[ "$pack_filename" == "$dry_filename" ]] || die "npm pack filename drifted for ${package_names[$index]}@${versions[$index]}"
  [[ "$pack_integrity" == "$dry_integrity" ]] || die "npm pack integrity drifted after dry-run for ${package_names[$index]}@${versions[$index]}"
  tarball="$tarball_dir/$pack_filename"
  if ! measured_integrity="$(file_integrity "$tarball")"; then
    die "could not hash packed tarball for ${package_names[$index]}@${versions[$index]}"
  fi
  [[ "$measured_integrity" == "$pack_integrity" ]] || die "packed tarball bytes disagree with npm integrity for ${package_names[$index]}@${versions[$index]}"
  local_integrities[$index]="$measured_integrity"
  tarballs[$index]="$tarball"
done

query_remote() {
  local spec="$1"
  local stdout_path stderr_path rc parsed
  stdout_path="$(mktemp "$work/view.XXXXXX")"
  stderr_path="$stdout_path.stderr"
  set +e
  npm view "$spec" dist.integrity --json >"$stdout_path" 2>"$stderr_path"
  rc=$?
  set -e
  if ! parsed="$(python3 - "$stdout_path" "$rc" <<'PY'
import base64
import binascii
import json
import sys

path, return_code_text = sys.argv[1:]
return_code = int(return_code_text)

def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result

try:
    with open(path, encoding="utf-8") as stream:
        payload = json.load(stream, object_pairs_hook=unique_object)
except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
    raise SystemExit("malformed npm view JSON: {}".format(error))

if return_code != 0:
    error = payload.get("error") if isinstance(payload, dict) else None
    if not isinstance(error, dict) or error.get("code") != "E404":
        raise SystemExit("npm view failed without a structured E404")
    print("missing")
    raise SystemExit(0)

if not isinstance(payload, str) or not payload.startswith("sha512-"):
    raise SystemExit("npm view did not return a sha512 integrity string")
try:
    digest = base64.b64decode(payload[len("sha512-"):], validate=True)
except (ValueError, binascii.Error) as error:
    raise SystemExit("npm view returned invalid sha512 integrity: {}".format(error))
if len(digest) != 64:
    raise SystemExit("npm view returned a sha512 integrity with the wrong digest length")
print("present\t" + payload)
PY
  )"; then
    [[ ! -s "$stderr_path" ]] || sed 's/^/npm view: /' "$stderr_path" >&2
    echo "error: could not classify registry state for $spec" >&2
    return 1
  fi
  printf '%s\n' "$parsed"
}

# Reconcile every remote version before publishing anything. This prevents a
# mismatch in a late platform or the wrapper from causing an avoidable partial
# release after earlier packages have already been published.
remote_states=()
for index in "${!package_dirs[@]}"; do
  spec="${package_names[$index]}@${versions[$index]}"
  if ! remote="$(query_remote "$spec")"; then
    die "registry preflight failed for $spec"
  fi
  case "$remote" in
    missing)
      remote_states[$index]="missing"
      ;;
    present$'\t'*)
      remote_integrity="${remote#*$'\t'}"
      if [[ "$remote_integrity" != "${local_integrities[$index]}" ]]; then
        die "refusing existing $spec: local integrity ${local_integrities[$index]} does not match remote integrity $remote_integrity"
      fi
      remote_states[$index]="identical"
      ;;
    *)
      die "unexpected registry classification for $spec"
      ;;
  esac
done

for index in "${!package_dirs[@]}"; do
  spec="${package_names[$index]}@${versions[$index]}"
  if [[ "${remote_states[$index]}" == "identical" ]]; then
    echo "skip: $spec already published with identical integrity"
    continue
  fi

  echo "publish: $spec"
  if ! npm publish "${tarballs[$index]}" --access public --provenance --ignore-scripts; then
    die "npm publish failed for $spec"
  fi

  verified=0
  for ((attempt = 1; attempt <= poll_attempts; attempt++)); do
    if ! remote="$(query_remote "$spec")"; then
      die "registry verification failed for newly published $spec"
    fi
    case "$remote" in
      missing)
        if (( attempt < poll_attempts )); then
          sleep "$poll_interval"
        fi
        ;;
      present$'\t'*)
        remote_integrity="${remote#*$'\t'}"
        if [[ "$remote_integrity" != "${local_integrities[$index]}" ]]; then
          die "published $spec has remote integrity $remote_integrity; expected ${local_integrities[$index]}"
        fi
        verified=1
        echo "verified: $spec integrity ${local_integrities[$index]}"
        break
        ;;
      *)
        die "unexpected registry classification while verifying $spec"
        ;;
    esac
  done
  [[ "$verified" -eq 1 ]] || die "published $spec did not become visible with the expected integrity after $poll_attempts attempts"
done

echo "npm package publication complete: $common_version"
