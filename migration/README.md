# Migration ledger

- `state.json`: upstream tracking markers (`baseline`, `last_analyzed`, `last_integrated`) and their history.
  Written by `coolify-port`; `last_integrated` moves only through `coolify-port mark-integrated` after a human merge.
- `ledger/<run-id>.json|.md`: one entry per migration run (upstream range, per-commit classification, plan, generated
  files, validation attempts, verdict, recommended action). Machine-local paths are stripped.
- `ledger/metrics.json`: per-run outcome metrics. Fill `review_outcome` (`accepted`, `rejected`, `regressed`) after
  review; it feeds calibration.

Full run artifacts (worktree, `patch.diff`, `prompt-pack.md`) live in `.migration-runs/` (git-ignored).
