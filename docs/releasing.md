# Releasing

Releases are semver-tagged: pushing `vMAJOR.MINOR.PATCH[-prerelease]` runs
`.github/workflows/release.yml`, which publishes crates.io + npm + PyPI,
uploads `cleverhans` service binaries (5 platforms + linux musl) to the
GitHub Release, and pushes the multi-arch service image to ghcr.io — in one
go. Nothing publishes until the `semver` gate passes.

## Cutting a release

One command, on the branch you release from:

```sh
scripts/set-version.sh 0.2.0
```

It bumps every manifest, commits `release: v0.2.0`, then prompts twice:
push the branch, and push the tag. The tag push is the trigger — answering
`N` keeps everything local (`git push origin v0.2.0` later publishes). The
script aborts before committing if the tag already exists.

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
and re-push the tag. Use **Re-run failed jobs** in the Actions UI:

- **crates** resumes — `scripts/publish-crates.sh` skips crates already on
  crates.io and waits out the new-crate rate limit (429), so a first
  release that publishes in waves just needs patience or a re-run.
- **pypi** resumes — `skip-existing` ignores files PyPI already has.
- **npm** is the strict one: re-publishing an existing version fails, so
  fix forward with a patch release if npm partially published.

## One-time registry setup

- **crates.io** — `CARGO_REGISTRY_TOKEN` secret in the `release` GitHub
  environment (crates.io has no OIDC yet).
- **npm** — the `cleverhans` org, then a **trusted publisher** on every
  package (main four + the five platform packages + `create-cleverhans`):
  org `2commits`, repo `cleverhans`, workflow `release.yml`, environment
  `release`. No token: npm requires 2FA for token publishes now, so CI
  authenticates via OIDC and provenance comes free. If a package doesn't
  exist yet and the UI won't accept a trusted publisher for it, publish
  its first version manually (`npm publish --access public` + OTP), then
  configure the trusted publisher.
- **PyPI** — trusted publisher for `cleverhans-hitl`: repo
  `2commits/cleverhans`, workflow `release.yml`, environment `release`.
