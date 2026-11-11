#!/usr/bin/env bash
# Merge upstream (origin = Indosaram/mhc) main into our main (fork = jc01rho/mhc) and push.
# Upstream .github/workflows changes are not taken: ours stay as they are.
# usage: sync-upstream.sh REPO_DIR   (a clone with remotes origin and fork; the checkout itself is never touched)
set -euo pipefail
repo=${1:?repo dir}
cd "$repo"
git fetch -q origin main
git fetch -q fork main
if git merge-base --is-ancestor origin/main fork/main; then echo "up to date"; exit 0; fi
tmp=$(mktemp -d)
trap 'git worktree remove --force "$tmp/w" 2>/dev/null || true; git branch -D sync-tmp 2>/dev/null || true; rm -rf "$tmp"' EXIT
git worktree add -q -B sync-tmp "$tmp/w" fork/main
cd "$tmp/w"
if ! git merge --no-edit -m "chore(sync): merge upstream main" origin/main; then
  git merge --abort || true
  echo "CONFLICT: upstream merge needs a person" >&2
  exit 2
fi
git checkout fork/main -- .github/workflows
git diff --cached --quiet || git commit -q -m "chore(sync): keep our workflows"
git push -q fork HEAD:main
echo "synced to $(git rev-parse --short HEAD)"
