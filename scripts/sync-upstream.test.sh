#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
sync_script="$script_dir/sync-upstream.sh"
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_TERMINAL_PROMPT=0

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

cat >"$root/ci-ours.yml" <<'YAML'
name: ci
on:
  push:
    branches: [main, "verify/**"]
  pull_request:
  workflow_dispatch:
jobs:
  test:
    runs-on: ubuntu-24.04
YAML

cat >"$root/release-ours.yml" <<'YAML'
name: release
on:
  push:
    tags: ["v*"]
jobs:
  publish:
    runs-on: ubuntu-24.04
YAML

cat >"$root/ci-upstream.yml" <<'YAML'
name: ci
on:
  workflow_dispatch:
jobs:
  test:
    runs-on: ubuntu-24.04
YAML

new_repos() {
  rm -rf "$root/work" "$root/seed" "$root/check1" "$root/check3" "$root/upstream.git" "$root/fork.git"
  git init -q --bare "$root/upstream.git"
  git init -q --bare "$root/fork.git"
  git -C "$root/upstream.git" symbolic-ref HEAD refs/heads/main
  git -C "$root/fork.git" symbolic-ref HEAD refs/heads/main
  git init -q "$root/seed"
  cd "$root/seed"
  mkdir -p .github/workflows crates
  cat >.github/workflows/ci.yml <<'YAML'
name: ci
on:
  push:
    branches: [main]
  workflow_dispatch:
jobs:
  test:
    runs-on: ubuntu-24.04
YAML
  cp "$root/release-ours.yml" .github/workflows/release.yml
  printf '# AGENTS\n\n## REMOTE BUILD ON GITHUB ACTIONS\ngh-verify.sh\n' >AGENTS.md
  printf '# Contributing\n\nscripts/gh-verify.sh\n' >CONTRIBUTING.md
  printf '# readme\n\nupstream text\n' >README.md
  echo "fn main() {}" >crates/lib.rs
  git add -A
  git commit -q -m base
  git branch -M main
  git remote add upstream "$root/upstream.git"
  git remote add fork "$root/fork.git"
  git push -q upstream main
  git push -q fork main
}

fork_ci_edit() {
  cd "$root/seed"
  git checkout -q main
  cp "$root/ci-ours.yml" .github/workflows/ci.yml
  git commit -q -am "our ci triggers"
  git push -q fork main
}

checkout_clone() {
  rm -rf "$root/work"
  git clone -q "$root/fork.git" "$root/work"
  cd "$root/work"
  git remote rename origin fork
  git remote add origin "$root/upstream.git"
}

test_workflow_conflict_only() {
  new_repos
  fork_ci_edit
  checkout_clone

  cd "$root/seed"
  git checkout -q -B up main~1
  cp "$root/ci-upstream.yml" .github/workflows/ci.yml
  printf 'name: release\non:\n  push:\n    tags: ["v*"]\n# upstream comment\njobs:\n  publish:\n    runs-on: ubuntu-24.04\n' >.github/workflows/release.yml
  printf 'name: brand-new\non:\n  workflow_dispatch:\n' >.github/workflows/brand-new.yml
  printf '# AGENTS\n\n## LOCAL VERIFICATION\nverify-local.sh\n' >AGENTS.md
  printf '# Contributing\n\nscripts/verify-local.sh\n' >CONTRIBUTING.md
  printf '# readme\n\nupstream text, changed\n' >README.md
  echo "fn added() {}" >>crates/lib.rs
  git add -A
  git commit -q -m "upstream changes"
  git push -q -f upstream HEAD:main

  if ! bash "$sync_script" "$root/work" >"$root/out1.log" 2>&1; then
    cat "$root/out1.log" >&2
    fail "workflow-only conflict should merge, script exited non-zero"
  fi

  git clone -q "$root/fork.git" "$root/check1"
  cd "$root/check1"
  cmp -s .github/workflows/ci.yml "$root/ci-ours.yml" || fail "our ci.yml was not kept verbatim"
  cmp -s .github/workflows/release.yml "$root/release-ours.yml" || fail "our release.yml was not kept verbatim"
  [ ! -e .github/workflows/brand-new.yml ] || fail "an upstream-added workflow file was taken"
  grep -q 'gh-verify.sh' AGENTS.md || fail "AGENTS.md was taken from upstream"
  grep -q 'gh-verify.sh' CONTRIBUTING.md || fail "CONTRIBUTING.md was taken from upstream"
  grep -q 'changed' README.md || fail "an ordinary upstream doc change was dropped"
  grep -q 'fn added' crates/lib.rs || fail "the upstream source change was not merged in"
  [ "$(git log -1 --format=%s)" = "chore(sync): merge upstream main" ] || fail "unexpected merge commit: $(git log -1 --format=%s)"
  git merge-base --is-ancestor "origin/main" HEAD || fail "upstream main is not an ancestor of the sync commit"

  checkout_clone
  bash "$sync_script" "$root/work" >"$root/out2.log" 2>&1 || { cat "$root/out2.log" >&2; fail "re-run failed"; }
  grep -q 'main up to date' "$root/out2.log" || fail "re-run did not report up to date"

  echo "PASS workflow conflict resolves on its own"
}

test_source_conflict_stops() {
  new_repos
  fork_ci_edit
  cd "$root/seed"
  echo "fn ours() {}" >crates/lib.rs
  git commit -q -am "our source"
  git push -q fork main

  git checkout -q -B up main~2
  echo "fn upstream() {}" >crates/lib.rs
  git commit -q -am "upstream source"
  git push -q -f upstream HEAD:main

  checkout_clone
  set +e
  bash "$sync_script" "$root/work" >"$root/out3.log" 2>&1
  status=$?
  set -e
  [ "$status" -eq 2 ] || { cat "$root/out3.log" >&2; fail "source conflict should exit 2, got $status"; }
  grep -q 'CONFLICT: upstream merge needs a person' "$root/out3.log" || fail "no person-needed message"
  grep -q 'crates/lib.rs' "$root/out3.log" || fail "the conflicting path was not named"

  git clone -q "$root/fork.git" "$root/check3"
  cd "$root/check3"
  [ "$(git log -1 --format=%s)" = "our source" ] || fail "fork main moved on a conflicting merge"

  echo "PASS non-workflow conflict stops without pushing"
}

test_workflow_conflict_only
test_source_conflict_stops
echo "OK sync-upstream.sh"
