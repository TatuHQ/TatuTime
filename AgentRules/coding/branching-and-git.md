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
- Merge only after the review requirement below and the required checks pass.
- Delete the branch — locally and on `origin` — once its pull request is merged.

## Protected `main`

Rulesets refuse direct pushes, force pushes, and deletion in every repository, so nothing reaches
`main` except through a pull request whose required checks pass. Never ask for, use, or work around
a bypass.

Who reviews depends on who authored the pull request:

| Author | Review required |
| --- | --- |
| The repository owner — the code owner in `.github/CODEOWNERS` | None; merge once checks pass |
| Anyone else, including agents and bots | The owner's approval |

Working for a non-owner, an agent opens the pull request and requests the owner's review. It never
approves work it did not author, never treats a bot or non-owner approval as the owner's, and never
merges that pull request or escalates to an admin flag until the owner approves.

## Commits

- Use conventional commit messages: `feat:`, `fix:`, `docs:`, `chore:`, `test:`.
- One logical change per commit; reference the ticket number where applicable.
- Rebase unpublished branches freely. Never rewrite shared history without explicit approval.

Secret handling is in [`../security/security.md`](../security/security.md).
