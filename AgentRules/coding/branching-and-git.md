---
load-when: creating a branch, committing, or merging a change
applies-to: every repository
title: Branching & git
---

# Branching and Git

Branch roles and merge targets follow Gitflow, and every repository keeps a `develop`
branch. Repoint a repository that lacks one before its next change, and do not commit to
`main` directly in the meantime.

| Branch | Created from | Merged into |
| --- | --- | --- |
| `feature/<name>` | `develop` | `develop` |
| `release/x.y.z` | `develop` | `main` and `develop` |
| `hotfix/<name>` | `main` | `main` and `develop` |

- Never commit directly to `main` or `develop`; every change merges through a pull request.
- Keep the branch up to date with the branch its pull request targets, and resolve conflicts
  locally before requesting review.
- Merge only after review and the required checks pass.
- Delete the branch — locally and on `origin` — once its pull request is merged.

## Commits

- Use conventional commit messages: `feat:`, `fix:`, `docs:`, `chore:`, `test:`.
- One logical change per commit; reference the ticket number where applicable.
- Rebase unpublished branches freely. Never rewrite shared history without explicit approval.

Secret handling is in [`../security/security.md`](../security/security.md).
