#!/usr/bin/env bash
# Takes the schema baseline for a release: copies the JSON Schemas in
# spec/schemas/ and spec/webhook/schemas/ over spec/baseline/ and resets
# spec/baseline/breaks.json to an empty list. Run it when a release is cut,
# before scripts/set-version.sh, and commit the result (docs/releasing.md).
#
#   scripts/snapshot-schemas.sh
#
# crates/cleverhans-core/tests/spec_schema_growth.rs compares each schema
# with this baseline and fails on a change that is not additive (SPEC.md
# §13), unless breaks.json accepts it. The copies are byte for byte; never
# edit them by hand.
set -euo pipefail

cd "$(dirname "$0")/.."

# The folders under spec/ that the baseline mirrors; the test lists the same
# ones in FOLDERS.
FOLDERS=(schemas webhook/schemas)

for folder in "${FOLDERS[@]}"; do
  if ! ls "spec/$folder"/*.json > /dev/null 2>&1; then
    echo "no schemas in spec/$folder — aborting before touching the baseline" >&2
    exit 1
  fi
done

# A schema removed since the last baseline leaves the baseline too: the
# new baseline is exactly what this release has.
for folder in "${FOLDERS[@]}"; do
  rm -rf "spec/baseline/$folder"
  mkdir -p "spec/baseline/$folder"
  cp -p "spec/$folder"/*.json "spec/baseline/$folder/"
done

# The breaks accepted since the last release are part of this release now.
printf '[]\n' > spec/baseline/breaks.json

echo "baseline taken:"
for folder in "${FOLDERS[@]}"; do
  echo "  spec/baseline/$folder: $(ls "spec/baseline/$folder"/*.json | wc -l | tr -d ' ') schemas"
done
echo "  spec/baseline/breaks.json: []"
if git rev-parse --is-inside-work-tree > /dev/null 2>&1; then
  git status --short spec/baseline
fi
