#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [[ -x "$repo_root/stemma" ]]; then
  default_stemma="$repo_root/stemma"
  default_input="$repo_root/demo/before.docx"
  default_approved="$repo_root/demo/approved-worklist.json"
  default_refused="$repo_root/demo/refused-worklist.json"
else
  default_stemma=stemma
  default_input="$repo_root/stemma-engine/testdata/simple-text/before.docx"
  default_approved="$repo_root/stemma-cli/examples/approved-worklist.json"
  default_refused="$repo_root/stemma-cli/examples/refused-worklist.json"
fi
stemma=${STEMMA:-$default_stemma}
input=${INPUT:-$default_input}
approved=${APPROVED_WORKLIST:-$default_approved}
refused=${REFUSED_WORKLIST:-$default_refused}
command -v jq >/dev/null || { printf 'jq is required\n' >&2; exit 1; }
command -v "$stemma" >/dev/null || { printf 'stemma executable is unavailable: %s\n' "$stemma" >&2; exit 1; }
for required in "$input" "$approved" "$refused"; do
  [[ -f "$required" ]] || { printf 'demo input is missing: %s\n' "$required" >&2; exit 1; }
done
demo_dir=$(mktemp -d)
trap 'rm -rf "$demo_dir"' EXIT

printf '1/4 Input text and approved worklist\n'
"$stemma" extract "$input" --format text
sed -n '1,80p' "$approved"

printf '2/4 Complete native redline and receipt\n'
"$stemma" apply "$input" --worklist "$approved" \
  -o "$demo_dir/redline.docx" --receipt "$demo_dir/receipt.json" \
  >"$demo_dir/stdout.json"
"$stemma" validate "$demo_dir/redline.docx"
jq '{status, deliverable, summary, output: .output.persistence_confirmation}' \
  "$demo_dir/receipt.json"

printf '3/4 Native revision inventory\n'
"$stemma" extract "$demo_dir/redline.docx" --format json \
  | jq '{revisions: [.revisions[] | {kind, author, block_id, excerpt}]}'

printf '4/4 Safe refusal: receipt exists, DOCX does not\n'
set +e
"$stemma" apply "$input" --worklist "$refused" \
  -o "$demo_dir/refused.docx" --receipt "$demo_dir/refused.receipt.json" \
  >"$demo_dir/refused.stdout.json"
status=$?
set -e
test "$status" -eq 3
test -f "$demo_dir/refused.receipt.json"
test ! -e "$demo_dir/refused.docx"
jq '{status, deliverable, summary, item: (.items[0] | {id, status, code, actual_matches})}' \
  "$demo_dir/refused.receipt.json"
