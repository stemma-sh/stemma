# stemma-examples e2e tests

Browser end-to-end tests for the Word-frontend editor, run with
[Playwright](https://playwright.dev/) against a live `stemma-api` server.

Every test's bar is **round-trip through the engine**: an edit must survive a
fresh `GET /rich` read (i.e. the engine actually persisted it), not just change
the local DOM. That's what makes these regression-meaningful rather than
snapshot-fragile.

## Run

```bash
cd stemma-examples/e2e
npm install            # once — installs Playwright + its browser
npx playwright install chromium   # if the browser isn't already present
./run.sh               # starts stemma-api, runs all suites, reports pass/fail
```

`run.sh` is repo-relative: it finds the workspace root, builds + starts
`stemma-api` (via `mise exec -- cargo` if `mise` is present, else `cargo`),
waits for it, runs the suites, and tears the server down. Set `PORT` to override
the default `3137`.

## Suites

| Suite | Covers |
|---|---|
| `run-editor`   | The core edit loop: open → render → edit a block → commit as a tracked `replace` → redline. |
| `run-latency`  | Optimistic commit: the redline shows in ms even with the server delayed, then reconciles. |
| `run-samples`  | All five bundled samples render (text, table, images, equations) without errors. |
| `run-format`   | B/I/U/S round-trip through the engine's `replace` content marks. |
| `run-features` | The authoring stack, each asserted to round-trip: run color + alignment (`set_format`/`set_para_format`), structural Enter/Backspace (`insert`/`delete`), table cell edit (`table_op`), bullet toggle (`set_numbering`), hyperlink, image insert (`insert_image`), and comments (`comment_create`). |

Each suite exits non-zero on failure, so `run.sh` aggregates a single result.

## Notes

- The server is started fresh per run; tests open their own documents, so they
  are independent and order-insensitive.
- `node_modules/`, screenshots, and `api.log` are git-ignored.
- These complement the engine's own Rust tests (`cargo test -p stemma`) — they
  exercise the *frontend + API + projection* path the unit tests don't reach.
