# Git hooks (merge/push quality gate)

Optional hooks that run the quality gate locally on merge/push to `main`.

- `pre-commit` — fast `cargo check --workspace` before every commit.
- `pre-merge-commit` — runs `just gate` (lint + all daily tests) when merging into `main`.
- `pre-push` — runs `just gate` when pushing `main`.

Install (one-time, per clone/worktree):

    git config core.hooksPath .githooks
