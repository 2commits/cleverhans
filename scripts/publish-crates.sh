#!/usr/bin/env bash
# Publishes the workspace crates to crates.io in dependency order,
# resiliently:
#
# - skips any crate whose current version is already on the registry, so
#   a partially-failed release can simply be re-run
# - waits out crates.io's new-crate rate limit (429: burst of a few new
#   names, then ~1 per 10 minutes) instead of dying mid-release
#
# Used by .github/workflows/release.yml; runs locally too (needs
# CARGO_REGISTRY_TOKEN or `cargo login`).
set -euo pipefail

cd "$(dirname "$0")/.."

# Dependency order: every crate only depends on ones before it.
CRATES=(
  cleverhans-core
  cleverhans-ws-core
  cleverhans-ws
  cleverhans-llm-anthropic
  cleverhans-llm-ollama
  cleverhans-evals
  cleverhans-codegen
  cleverhans
)

VERSION="$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)"
echo "publishing workspace version $VERSION"

published() {
  # Sparse index path for names >= 4 chars: {first2}/{next2}/{name}.
  local name="$1"
  curl -fsL "https://index.crates.io/${name:0:2}/${name:2:2}/${name}" 2>/dev/null \
    | grep -q "\"vers\":\"${VERSION}\""
}

for crate in "${CRATES[@]}"; do
  if published "$crate"; then
    echo "== $crate@$VERSION already on crates.io — skipping"
    continue
  fi
  attempt=1
  while true; do
    echo "== publishing $crate@$VERSION (attempt $attempt)"
    if cargo publish -p "$crate" 2>&1 | tee /tmp/cargo-publish.log; then
      break
    fi
    if grep -q '429 Too Many Requests' /tmp/cargo-publish.log && [ "$attempt" -lt 8 ]; then
      echo "== rate limited — sleeping 10.5 minutes before retrying $crate"
      sleep 630
      attempt=$((attempt + 1))
    else
      echo "== $crate failed for a non-rate-limit reason (or too many attempts)" >&2
      exit 1
    fi
  done
done

echo "all crates published at $VERSION"
