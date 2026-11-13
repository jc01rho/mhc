#!/usr/bin/env bash
# Merge upstream (origin = Indosaram/mhc) main into our main (fork = jc01rho/mhc) and push.
# Then tag our main with the newest upstream release tag as <tag>-omomeow1 (once) and start release.yml for it.
# Fork-owned CI surface (FORK_OWNED) is never taken from upstream: our workflows, our
# GitHub-Actions verify/install scripts and the agent docs that describe them stay ours,
# and a conflict inside that surface is expected rather than a reason to stop.
# usage: sync-upstream.sh REPO_DIR [--dry-run]   (a clone with remotes origin and fork; the checkout itself is never touched)
set -euo pipefail
repo=${1:?repo dir}
dry=${2:-}
cd "$repo"
git fetch -q origin main
git fetch -q fork main

FORK_OWNED=(.github/workflows scripts/gh-verify.sh scripts/gh-install.sh AGENTS.md CONTRIBUTING.md)

restore_fork_owned() {
  local path
  for path in "${FORK_OWNED[@]}"; do
    git cat-file -e "fork/main:$path" 2>/dev/null || continue
    rm -rf "$path"
    git checkout fork/main -- "$path"
    git add -A "$path"
  done
}

is_fork_owned() {
  local path=$1 owned
  for owned in "${FORK_OWNED[@]}"; do
    if [ "$path" = "$owned" ] || [ "${path#"$owned"/}" != "$path" ]; then
      return 0
    fi
  done
  return 1
}

merge_upstream() {
  tmp=$(mktemp -d)
  trap 'git worktree remove --force "$tmp/w" 2>/dev/null || true; git branch -D sync-tmp 2>/dev/null || true; rm -rf "$tmp"' EXIT
  git worktree add -q -B sync-tmp "$tmp/w" fork/main
  cd "$tmp/w"
  if ! git merge --no-commit --no-ff origin/main; then
    if [ ! -f "$(git rev-parse --git-path MERGE_HEAD)" ]; then
      echo "FAILED: upstream merge did not start" >&2
      exit 2
    fi
    other=()
    while IFS= read -r path; do
      [ -n "$path" ] || continue
      is_fork_owned "$path" || other+=("$path")
    done < <(git diff --name-only --diff-filter=U)
    if [ "${#other[@]}" -gt 0 ]; then
      git merge --abort || true
      echo "CONFLICT: upstream merge needs a person: ${other[*]}" >&2
      exit 2
    fi
    echo "conflict inside the fork-owned CI surface only: keeping ours"
  fi
  restore_fork_owned
  git commit -q -m "chore(sync): merge upstream main"
  git push -q fork HEAD:main
  echo "synced to $(git rev-parse --short HEAD)"
  cd "$repo"
  git fetch -q fork main
}

if git merge-base --is-ancestor origin/main fork/main; then
  echo "main up to date"
elif [ -n "$dry" ]; then
  echo "would merge upstream main"
else
  merge_upstream
fi

# Newest upstream release tag; only the newest one is released, and only once.
newest=$(git ls-remote --tags --refs origin 'refs/tags/*' | awk -F'refs/tags/' '{print $2}' | sort -V | tail -1)
if [ -n "$newest" ]; then
  ours="$newest-omomeow1"
  if [ -z "$(git ls-remote --tags fork "refs/tags/$ours")" ]; then
    if [ -n "$dry" ]; then
      echo "would tag $(git rev-parse --short fork/main) as $ours"
    else
      git tag "$ours" fork/main
      git push -q fork "refs/tags/$ours"
      # A tag pushed with GITHUB_TOKEN does not start release.yml, so dispatch it explicitly.
      gh workflow run release.yml -f "tag=$ours"
      echo "tagged $ours and started release"
    fi
  else
    echo "$ours already released"
  fi
fi
