#!/usr/bin/env bash
# Cuts a release: sets the version across every manifest, commits, and
# (each behind a y/N prompt) pushes the branch and pushes the vX.Y.Z tag —
# the tag push is what triggers the publish workflow.
#
#   scripts/set-version.sh 0.2.0
#   scripts/set-version.sh 0.2.0-rc.1
#
# Touches: [workspace.package] version + the internal cleverhans-* dep
# versions in Cargo.toml, Cargo.lock, the four publishable package.jsons,
# and pyproject.toml. The release workflow's `semver` job refuses any tag
# whose version doesn't match all of these.
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="${1:-}"
if ! echo "$VERSION" | grep -Eq \
  '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'; then
  echo "usage: scripts/set-version.sh <semver>   (e.g. 0.2.0 or 0.2.0-rc.1)" >&2
  exit 1
fi

# Cargo.toml: [workspace.package] version + internal dep pins.
perl -pi -e '
  s{^version = "[^"]+"$}{version = "'"$VERSION"'"} if $. > 1 && $seen_wp;
  $seen_wp = 1 if /^\[workspace\.package\]/;
  $seen_wp = 0 if /^\[/ && !/^\[workspace\.package\]/;
  s{^(cleverhans[a-z-]* = \{ path = "[^"]+", version = ")[^"]+(" \})$}{${1}'"$VERSION"'${2}};
' Cargo.toml

# Python distribution (first `version =` line only — the project version).
perl -pi -e '$done ||= s{^version = "[^"]+"$}{version = "'"$VERSION"'"};' \
  python/cleverhans-hitl/pyproject.toml

# npm packages.
for p in cleverhans-react cleverhans-ui cleverhans-node create-cleverhans; do
  node -e '
    const fs = require("fs");
    const path = `typescript/'"$p"'/package.json`;
    const pkg = JSON.parse(fs.readFileSync(path, "utf8"));
    pkg.version = process.argv[1];
    fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + "\n");
  ' "$VERSION"
done

# Refresh Cargo.lock for the workspace members.
cargo update --workspace --quiet

echo "set $VERSION:"
grep -m1 '^version = ' Cargo.toml
grep -m1 '^version = ' python/cleverhans-hitl/pyproject.toml
for p in cleverhans-react cleverhans-ui cleverhans-node create-cleverhans; do
  echo "typescript/$p: $(node -p "require('./typescript/$p/package.json').version")"
done

confirm() {
  local answer
  read -r -p "$1 [y/N] " answer
  [ "$answer" = "y" ] || [ "$answer" = "Y" ]
}

# Stage exactly the files this script touches, commit, tag.
TAG="v$VERSION"
BRANCH="$(git branch --show-current)"
if git rev-parse -q --verify "refs/tags/$TAG" > /dev/null; then
  echo "tag $TAG already exists — aborting before commit" >&2
  exit 1
fi

git add Cargo.toml Cargo.lock python/cleverhans-hitl/pyproject.toml \
  typescript/cleverhans-react/package.json typescript/cleverhans-ui/package.json \
  typescript/cleverhans-node/package.json typescript/create-cleverhans/package.json
if git diff --cached --quiet; then
  echo "(manifests already at $VERSION — nothing to commit)"
else
  git commit -m "release: $TAG"
fi

if confirm "push $BRANCH to origin?"; then
  git push origin "$BRANCH"
else
  echo "(branch not pushed — the tag push below still uploads the release commit,"
  echo " but $BRANCH on origin will not contain it)"
fi

git tag "$TAG"
echo "tagged $TAG"

if confirm "push $TAG to origin? (this publishes to crates.io, npm, and PyPI)"; then
  git push origin "$TAG"
  echo "release running: https://github.com/2commits/cleverhans/actions"
else
  echo "(tag kept local — publish later with: git push origin $TAG)"
fi
