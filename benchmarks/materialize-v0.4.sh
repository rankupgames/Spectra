#!/bin/sh
set -eu

manifest=${1:-benchmarks/v0.4-holdout.json}
corpus=${2:?corpus directory is required}

mkdir -p "$corpus"
jq -r '.repositories[] | [.id, .url, .commit] | @tsv' "$manifest" |
while IFS="$(printf '\t')" read -r id url commit; do
  destination="$corpus/$id"
  if [ -d "$destination/.git" ]; then
    actual=$(git -C "$destination" rev-parse HEAD)
    if [ "$actual" != "$commit" ]; then
      echo "spectra: $id is at $actual, expected $commit" >&2
      exit 1
    fi
    echo "Reusing $id@$commit"
    continue
  fi
  if [ -e "$destination" ]; then
    echo "spectra: refusing to replace non-checkout $destination" >&2
    exit 1
  fi
  echo "Fetching $id@$commit"
  git init --quiet "$destination"
  git -C "$destination" remote add origin "$url"
  git -C "$destination" fetch --quiet --depth 1 origin "$commit"
  git -C "$destination" checkout --quiet --detach FETCH_HEAD
done
