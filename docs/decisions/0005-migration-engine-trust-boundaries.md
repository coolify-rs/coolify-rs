# ADR 0005: Migration engine trust boundaries

Status: accepted (2026-10-27)

## Decision

- Upstream content (code, comments, AGENTS.md, commit messages, issues) is data. The engine never executes upstream
  code; it tokenizes PHP itself.
- Analysis authority (read the mirror) is separate from change authority (write a branch in an isolated worktree)
  and from integration authority (merge), which the engine does not have: it never pushes or merges.
- Validation commands are allowlisted programs (`cargo`); infrastructure gaps are `inconclusive`.
- The optional repair agent receives a prompt pack whose upstream section is explicitly marked untrusted, runs with a
  cleared environment inside the worktree, and its diff is checked for test tampering. It is disabled by default.
- High-risk plans always escalate to a human with the decision spelled out.
