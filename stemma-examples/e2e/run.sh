#!/usr/bin/env bash
# End-to-end tests for the stemma Word-frontend example.
#
# Starts a `stemma-api` server, runs the Playwright suites against it, and
# reports a pass/fail per suite. Repo-relative — run from anywhere.
#
#   cd stemma-examples/e2e && npm install   # once, to get Playwright
#   ./run.sh                                 # run all suites
#
# Env: PORT (default 3137). Requires `cargo` (via mise or PATH) and node.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# workspace root = two levels up from stemma-examples/e2e (…/<workspace>)
ROOT="$(cd "$HERE/../.." && pwd)"
PORT="${PORT:-3137}"
BASE="http://127.0.0.1:${PORT}"
DOCX="$ROOT/stemma-examples/samples/safe-agreement.docx"
CARGO="cargo"; command -v mise >/dev/null 2>&1 && CARGO="mise exec -- cargo"

if [ ! -d "$HERE/node_modules/playwright" ]; then
  echo "Playwright not installed — run \`npm install\` in $HERE first." >&2
  exit 3
fi

pkill -f "target/debug/stemma-api" 2>/dev/null; sleep 1
( cd "$ROOT" && STEMMA_API_PORT="$PORT" $CARGO run -q -p stemma-api >"$HERE/api.log" 2>&1 ) &
SRV=$!
trap 'kill $SRV 2>/dev/null; pkill -f "target/debug/stemma-api" 2>/dev/null' EXIT

up=0
for _ in $(seq 1 60); do curl -s -o /dev/null "$BASE/" && { up=1; break; }; sleep 0.5; done
[ "$up" = 1 ] || { echo "SERVER DID NOT START"; tail -20 "$HERE/api.log"; exit 2; }
echo "server up on $BASE"

cd "$HERE"
rc=0
run() { echo "==================== $1 ===================="; shift; DOCX="$DOCX" BASE="$BASE" node "$@" || rc=1; }
run EDITOR   run-editor.mjs
run LIVE     run-live-track.mjs
run REEDIT   run-reedit.mjs
run TABLE    run-table-cell.mjs
run LATENCY  run-latency.mjs
run SAMPLES  run-samples.mjs
run FORMAT   run-format.mjs
run FEATURES run-features.mjs

echo "============================================="
[ $rc -eq 0 ] && echo "ALL SUITES PASSED" || echo "SOME SUITES FAILED"
exit $rc
