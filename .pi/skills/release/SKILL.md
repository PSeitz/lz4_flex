---
name: release
description: Prepare and publish an lz4_flex release from its major.minor.x release branch, including optional cherry-picks, version and changelog updates, a changelog PR to main, tagging, and cargo publish. Use when asked to cut, prepare, tag, or publish a release.
compatibility: Requires git, cargo, and GitHub CLI (gh); cargo publish requires crates.io credentials.
---

# Release lz4_flex

Run this workflow from the lz4_flex repository. Treat releases as resumable: inspect the repository and remote state before every phase, and skip only work that is verifiably complete.

## Safety rules

- Never discard, overwrite, stash, or include unrelated user changes.
- Never force-push a release branch or move an existing tag.
- Use release tags without a `v` prefix (for example, `0.13.1`).
- Stop on conflicts, failed checks, unexpected branch divergence, an existing mismatched tag/version, or an unexpectedly broad PR diff. Explain the state and ask how to proceed.
- Show the exact `cargo publish` command and ask for explicit confirmation immediately before running it. A prior request to perform a release is not publish confirmation.

## 1. Establish the release

Inspect `CHANGELOG.md`, the root `Cargo.toml`, recent tags, branches, remotes, status, and worktrees:

```bash
git status --short --branch
git remote -v
git fetch origin --prune --tags
git worktree list
git branch --all --no-color
git tag --sort=-version:refname | head -20
```

Find the version marked `unreleased` at the top of `CHANGELOG.md` and propose it as the default. Ask the user:

1. Which exact SemVer version should be released?
2. Are any commits or PRs to be cherry-picked? If yes, request their commit SHAs in the intended order.

Always wait for the version answer. For version `X.Y.Z`, derive release branch `X.Y.x`. Validate that the version, changelog, and branch agree.

## 2. Enter the release branch

Use the existing clean worktree for `X.Y.x` when one exists. Otherwise, from a clean worktree, check out the local branch or create it tracking `origin/X.Y.x`. Do not attempt to check out a branch already checked out in another worktree.

Confirm:

```bash
git branch --show-current
git status --short --branch
git log --oneline --decorate -10
```

The branch must be `X.Y.x`, clean apart from changes created by this release workflow, and based on the expected remote branch. Fast-forward from `origin/X.Y.x` when possible; do not rebase or reset divergent work without explicit approval.

## 3. Apply backports

If the user supplied commits, inspect each commit and confirm it is not already present. Cherry-pick them one at a time, in the supplied order:

```bash
git cherry-pick <sha>
```

After each pick, review the diff and run relevant targeted tests. On conflict, stop and ask rather than guessing at a semantic resolution. Do not cherry-pick merge commits without first agreeing on the correct `-m` parent.

## 4. Bump the crate version

Update the root `Cargo.toml` package version to exactly `X.Y.Z`. Do not accidentally change dependency versions or the independent `lz4_bin` package version. If a tracked `Cargo.lock` exists, regenerate it with Cargo and verify that only the intended package entry changed.

Search all manifests for other references that genuinely need synchronization, then review the diff. Do not blindly replace every occurrence of the old version.

## 5. Finalize `CHANGELOG.md`

Turn the matching unreleased heading into a release heading using today's ISO date:

```text
X.Y.Z (YYYY-MM-DD)
==================
```

If the release branch has no unreleased section, add the new entry at the top. Build the notes from the release commits and user-provided context; preserve the changelog's existing style, headings, links, credits, and security wording. Do not invent issue numbers or claims.

Show the proposed release notes to the user if their content is not already unambiguous.

## 6. Verify and commit the release preparation

Review all changes and run practical release checks, normally:

```bash
cargo fmt --check
cargo test --all-features
cargo package
```

Also run any repository-specific checks indicated by CI or the changed code. If `cargo package` fails only because the working tree is dirty, commit first and then rerun it; never use `--allow-dirty` to hide unrelated changes.

Present the exact files and commit message and ask before committing. Stage only explicit release files. A normal message is:

```text
release X.Y.Z
```

Push the release branch normally after approval; never force-push.

## 7. Update the changelog on `main`

Ensure `main` receives the finalized changelog through a PR. First inspect the relationship between `X.Y.x` and `origin/main` and review the complete proposed PR diff.

- If merging the release branch into `main` carries only intended changes, open a PR from `X.Y.x` to `main`.
- If that PR would downgrade development code, change the wrong manifest version, or include release-only changes, create a clean temporary branch from current `origin/main`, apply only the finalized `CHANGELOG.md` update (cherry-pick without committing and retain only the intended changelog change, or edit it directly), commit it, push it, and open that branch's PR to `main`.

Use `gh pr create --base main` with a concise title and a body summarizing the release and checks. Show the proposed title, body, and diff before creating the PR. If a suitable PR already exists, report it instead of opening a duplicate.

Do not continue to tagging until the changelog PR is merged, unless the user explicitly chooses to proceed before merge. If it is not merged yet, report the PR URL and stop; the workflow can resume later.

## 8. Tag the release

Return to the clean `X.Y.x` worktree, fetch the remote, and verify all of the following:

- `Cargo.toml` says `X.Y.Z`.
- `CHANGELOG.md` contains the dated `X.Y.Z` entry.
- The release commit is present on `origin/X.Y.x`.
- The changelog PR is merged (unless explicitly waived).
- Tag `X.Y.Z` does not already exist locally or remotely.
- Release checks passed on the exact commit being tagged.

Follow the repository's existing lightweight-tag convention:

```bash
git tag X.Y.Z
git push origin X.Y.Z
```

If the tag already exists and points at the expected commit, do not recreate it; report that tagging is complete. If it points elsewhere, stop.

## 9. Publish to crates.io

Run a dry run against the exact tagged checkout when practical:

```bash
cargo publish --dry-run
```

Then show the exact command:

```bash
cargo publish
```

Ask: **“Publish lz4_flex X.Y.Z to crates.io now?”** Wait for an explicit yes immediately before executing it.

After publishing, report the command result and verify the published version when practical (for example with `cargo search lz4_flex`). Do not retry an ambiguous publish automatically; first check whether crates.io accepted it.

## Completion report

Summarize:

- version, release branch, release commit, and tag
- cherry-picked commits
- changelog PR URL and merge state
- checks run and any skipped checks
- tag push state
- crates.io publication state
- any remaining manual action
