#!/usr/bin/env bash
# One-time npm bootstrap: publishes the first version of every package from
# your machine (npm prompts your 2FA OTP — that's the point: trusted
# publishers can only be configured on packages that already exist).
#
#   npm login                          # once, in a browser
#   scripts/bootstrap-npm.sh <run-id>  # a release run whose npm-native matrix succeeded
#
# The run-id supplies the five prebuilt platform binaries (they are not
# version-stamped, so any successful npm-native matrix works). After this
# script, add a trusted publisher (repo 2commits/cleverhans, workflow
# release.yml, environment release) to every package on npmjs.com, and CI
# owns all future publishes.
set -euo pipefail

cd "$(dirname "$0")/../typescript"

RUN_ID="${1:?usage: scripts/bootstrap-npm.sh <github-actions-run-id>}"
VERSION="$(node -p "require('./cleverhans-node/package.json').version")"
echo "bootstrapping npm packages at $VERSION"

pnpm install --frozen-lockfile
pnpm -r build

# Native binaries for all five platforms, from CI.
rm -rf cleverhans-node/artifacts
gh run download "$RUN_ID" --repo 2commits/cleverhans \
  --pattern 'bindings-*' --dir cleverhans-node/artifacts

# Prepare only: napi injects optionalDependencies + copies binaries, but
# must not publish itself — its captured `npm publish` cannot complete the
# interactive OTP/browser auth this bootstrap depends on.
(cd cleverhans-node \
  && pnpm exec napi create-npm-dirs \
  && pnpm exec napi artifacts --output-dir artifacts \
  && pnpm exec napi prepublish -t npm --no-gh-release --skip-optional-publish)

published() {
  npm view "$1@$VERSION" version > /dev/null 2>&1
}

# Platform packages, interactively (npm opens the browser / prompts OTP).
for dir in cleverhans-node/npm/*/; do
  name="$(node -p "require('./$dir/package.json').name")"
  if published "$name"; then
    echo "== $name@$VERSION already on npm — skipping"
    continue
  fi
  (cd "$dir" && npm publish --access public)
done

# Main packages: pnpm pack applies publishConfig + rewrites workspace:*.
for p in cleverhans-react cleverhans-ui cleverhans-node create-cleverhans; do
  name="$(node -p "require('./$p/package.json').name")"
  if published "$name"; then
    echo "== $name@$VERSION already on npm — skipping"
    continue
  fi
  (cd "$p" && pnpm pack --out package.tgz && npm publish package.tgz --access public)
done

echo
echo "done. Now add the trusted publisher on npmjs.com to each package:"
echo "  @cleverhans/react ui node node-* create-cleverhans"
echo "  (org 2commits, repo cleverhans, workflow release.yml, environment release)"
