# Releasing

Releases are semver-tagged: pushing `vMAJOR.MINOR.PATCH[-prerelease]` runs
`.github/workflows/release.yml`, which publishes crates.io + npm + PyPI in
one go. Nothing publishes until the `semver` gate passes.

## Cutting a release

```sh
# 1. Bump every manifest to the new version (one command):
scripts/set-version.sh 0.2.0

# 2. Commit, PR, merge to main as usual.

# 3. Tag the merge commit and push the tag:
git checkout main && git pull
git tag v0.2.0
git push origin v0.2.0
```

## What the `semver` gate enforces

- The tag is strict semver (`v1.2.3`, `v1.2.3-rc.1`, `v1.2.3-beta.2+build`);
  anything else fails before any registry is touched.
- Every manifest version equals the tag: `Cargo.toml`
  (`[workspace.package]`), the four npm `package.json`s, and
  `pyproject.toml`. A mismatch fails the whole run up front — no partial
  publish with wrong metadata.

## Prereleases

`v0.2.0-rc.1` runs the same pipeline with prerelease semantics:

- **npm** — published under the `next` dist-tag; `npm install
  @cleverhans/node` keeps resolving the last stable, `@next` opts in.
- **crates.io / PyPI** — prerelease versions are opt-in natively (cargo
  needs an explicit prerelease requirement; pip needs `--pre`).

Prereleases publish for real — versions are permanent on all three
registries — but they never move the stable pointer.

## If a job fails mid-release

Jobs are independent and registry versions are immutable, so never delete
and re-push the tag. Use **Re-run failed jobs** in the Actions UI; already-
published registries reject duplicates and the rest proceeds.

## One-time registry setup

`release` GitHub environment with `CARGO_REGISTRY_TOKEN` + `NPM_TOKEN`;
`@cleverhans` org on npm; PyPI trusted publisher for `cleverhans-hitl`
(repo `2commits/cleverhans`, workflow `release.yml`, environment
`release`).
