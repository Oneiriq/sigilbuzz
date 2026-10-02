# Releasing

How to cut a release of the sigilbuzz workspace and publish it to crates.io. All 12
crates are Apache-2.0 and ship from one git tag, but each one is its own package on
crates.io with its own version number.

## Versions

- The root crate `sigilbuzz` and its pin in `[workspace.dependencies]` always move
  together. Bump both.
- A companion crate gets a new version when it changed in the release. Before 1.0,
  bump the minor version for API changes and the patch version for everything else.
- Once the crates are on crates.io, a companion crate that exposes sigilbuzz types in
  its API (for example a function that takes `&Face`) needs a minor bump every time the
  core crate gets one. Otherwise its published version keeps depending on the older,
  incompatible core.
- The minimum supported Rust version is `rust-version` in `[workspace.package]`. If you
  raise it, say so in the changelog.

## Checklist

1. Move the "unreleased" section of [CHANGELOG.md](../CHANGELOG.md) to the new version
   and date.
2. Bump versions as described above.
3. Run the gate. The pre-push hook runs the same commands, so this is also what
   happens on push. Never bypass the hook with `--no-verify`.

   ```bash
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo clippy --workspace --no-default-features --all-targets -- -D warnings
   cargo test --workspace --all-features
   cargo build --no-default-features
   ```

4. Dry-run the publish:

   ```bash
   cargo publish --workspace --dry-run
   ```

   This packages every crate, then builds each one from its packaged form in
   dependency order. Workspace crates that are not on crates.io yet resolve locally, so
   the dry run works before the first real publish.

5. Merge to `main`, then tag the release commit:

   ```bash
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z
   ```

6. Create the GitHub release and paste this version's changelog section as the notes:

   ```bash
   gh release create vX.Y.Z --title vX.Y.Z --notes-file notes.md
   ```

7. Publishing the release starts the "Publish to crates.io" workflow
   (`.github/workflows/publish.yml`). It checks that the tag matches the `sigilbuzz`
   version, reruns the gate, `cargo audit`, and the dry run on the tagged commit, then
   waits for approval in the `crates-io-approval` environment. Once approved, it runs
   `cargo publish --workspace` with the `CARGO_REGISTRY_TOKEN` secret. A prerelease
   runs the checks only.

   crates.io accepts five new crates at once, then one every ten minutes, so a release
   that adds crates waits out that limit (the first release, which adds all 12, takes
   well over an hour). The job skips crates that are already up, so re-running a
   failed job resumes the release. To publish by hand instead, run
   `cargo publish --workspace` from the tagged commit after `cargo login`, and if it
   stops partway, publish the rest with `cargo publish -p <crate>`, following the
   order below.

8. Add the crates.io links to the GitHub release notes.

## Publish order

Crates in the same group don't depend on each other and can go in any order.

1. `sigilbuzz`
2. `sigilbuzz-paint`, `sigilbuzz-gpu`, `sigilbuzz-pdf`, `sigilbuzz-subset`,
   `sigilbuzz-woff`, `sigilbuzz-text-layout`
3. `sigilbuzz-render` (needs paint), `sigilbuzz-svg` (paint), `sigilbuzz-capi` (subset
   and paint), `sigilbuzz-hyphen` (text-layout)
4. `sigilbuzz-cli` (subset, paint, gpu, svg, pdf, and woff)

## Path plus version dependencies

Workspace crates depend on each other with both a path and a version, for example
`sigilbuzz = { path = ".", version = "0.22.0" }`. Cargo uses the path when building
locally and the version when packaging for crates.io, so there is nothing to strip
before publishing. When you bump a crate, update the version in every place that
depends on it. The dry run fails if they disagree.

## Package size

The core crate packs to about 3.2 MiB compressed, mostly test fonts. The crates.io
limit is 10 MiB. If a new fixture pushes it close, add an `exclude` list to the
package manifest.
