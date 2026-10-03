#!/usr/bin/env bash
# Publish measure.py's results to the data branch (spec "Where the data lives").
#
# Usage: push.sh bench|calibrate. Environment: DATA_BRANCH (default bench-data), RESULTS (default results),
# RUNNER_TEMP, GITHUB_SHA. Runs inside a checkout made by actions/checkout, whose credentials the worktree shares.
#
# A rejected push is never merged: the loop re-fetches the branch and redoes the whole update on its new contents, up
# to five times, because git cannot merge two edits of a JSON file safely.
set -euo pipefail

mode="$1"
branch="${DATA_BRANCH:-bench-data}"
results="${RESULTS:-results}"
flag="${RUNNER_TEMP:-/tmp}/needs-calibration"

git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"

for attempt in 1 2 3 4 5; do
  rm -rf data "$flag"
  git worktree prune
  if git ls-remote --exit-code --heads origin "$branch" > /dev/null; then
    git fetch --depth 1 origin "$branch"
    git worktree add --detach data FETCH_HEAD
  else
    git worktree add --detach data
    git -C data checkout --orphan bench-data-new
    git -C data rm -rfq .
  fi
  if [ "$mode" = bench ]; then
    python benches/publish/publish.py bench --data data --results "$results" --calibrate-flag "$flag"
  else
    python benches/publish/publish.py calibrate --data data --results "$results"
  fi
  git -C data add -A
  if git -C data diff --cached --quiet; then
    echo "nothing to publish"
    exit 0
  fi
  git -C data commit -qm "bench: $mode at ${GITHUB_SHA:-unknown}"
  if git -C data push origin "HEAD:refs/heads/$branch"; then
    # Only the real data branch is served. A dispatch with the job's GITHUB_TOKEN always starts a run (GitHub's docs:
    # "workflow_dispatch and repository_dispatch events always create workflow runs"); see plan decision 9.
    if [ "$branch" = bench-data ]; then
      if [ "$mode" = bench ] && [ -s "$flag" ]; then
        echo "a runner image is not yet calibrated; starting a calibration"
        gh workflow run bench-calibrate.yml
      fi
      echo "redeploying the site with the new data"
      gh workflow run docs.yml
    fi
    exit 0
  fi
  echo "push rejected (attempt $attempt of 5): re-fetching and redoing the update"
done
echo "gave up after five rejected pushes" >&2
exit 1
