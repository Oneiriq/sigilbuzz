# Releasing sigilbuzz to crates.io

This is the operator runbook for cutting a release of the sigilbuzz
workspace and pushing it to crates.io. Every workspace member ships
under the same version line and the same Apache-2.0 license, but
each crate is published as its own artifact and crates.io requires
them in dependency order.

## Publish order

The companion crates depend on the shaper, so `sigilbuzz` itself has
to land on the registry before any of them. Within the companion
group, `sigilbuzz-svg` has an optional dependency on `sigilbuzz-paint`
and `sigilbuzz-render` has a hard dependency on `sigilbuzz-paint`, so
both trail it. The full topological order is:

1. **`sigilbuzz`** — the shaping core. No workspace dependencies.
2. **`sigilbuzz-paint`** — depends on `sigilbuzz`.
3. **`sigilbuzz-gpu`** — depends on `sigilbuzz`.
4. **`sigilbuzz-render`** — depends on `sigilbuzz` and `sigilbuzz-paint`.
5. **`sigilbuzz-svg`** — depends on `sigilbuzz`; optional dep on `sigilbuzz-paint`.
6. **`sigilbuzz-pdf`** — depends on `sigilbuzz`.

After step 1 publishes, allow a minute or two for crates.io's index to
propagate before kicking off step 2 — otherwise the dependency
resolution for the companions will fail to find the new sigilbuzz
version.

## Dry-run before each release

Before tagging anything, sanity-check every member with a dry-run:

```bash
for crate in sigilbuzz sigilbuzz-paint sigilbuzz-gpu sigilbuzz-render sigilbuzz-svg sigilbuzz-pdf; do
    cargo publish -p "$crate" --dry-run --no-verify --allow-dirty
done
```

`--no-verify` skips the rebuild for speed; drop it for the actual
release commit so cargo verifies the packaged tarball compiles
end-to-end.

### Bootstrap caveat

Until `sigilbuzz` itself has at least one published version on
crates.io, the companion-crate dry-runs will fail with
`no matching package named 'sigilbuzz' found / location searched:
crates.io index`. This is expected — cargo resolves
`{ workspace = true }` deps against the real registry during
packaging. It is **not** a metadata bug in the companion crate.
After the first real `cargo publish -p sigilbuzz` lands, every
companion's dry-run will pass.

## Workflow: tag → release → publish

1. **Bump versions.** Edit the `[package]` `version` line in the root
   `Cargo.toml` and each `crates/*/Cargo.toml`. The companions track
   their own version, but in practice we bump them all in lockstep.
   Update `[workspace.dependencies] sigilbuzz` to match.
2. **Test gate.** `cargo test --workspace` and clippy (default +
   `--no-default-features`) must be green. The pre-push hook enforces
   this; never bypass it with `--no-verify`.
3. **Tag.** Cut a git tag of the form `vX.Y.Z` on the `main` branch
   commit that bumps the versions:
   ```bash
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z
   ```
4. **GitHub release.** `gh release create vX.Y.Z --generate-notes`
   to produce a draft release from the tag, then edit the notes for
   release-worthy items (breaking changes, headline features).
5. **Publish.** From the tagged commit, in the order above:
   ```bash
   cargo publish -p sigilbuzz
   # wait ~60s for index propagation
   cargo publish -p sigilbuzz-paint
   cargo publish -p sigilbuzz-gpu
   cargo publish -p sigilbuzz-render
   cargo publish -p sigilbuzz-svg
   cargo publish -p sigilbuzz-pdf
   ```
   Each `cargo publish` rebuilds the crate, packs it, and pushes to
   crates.io. Drop `--no-verify` here.
6. **Announce.** Edit the GitHub release notes to mention the
   crates.io URLs once they are live.

## Path + version dependencies

`sigilbuzz`'s entry in `[workspace.dependencies]` is
`{ path = ".", version = "X.Y.Z" }` and `sigilbuzz-svg`'s optional
`sigilbuzz-paint` dep follows the same shape. cargo uses the path
locally and the version when packaging for crates.io, so you do not
need to strip `path` at publish time. Verify with
`cargo publish --dry-run` if in doubt.

## What this PR does not do

`feature/crates-publish` only flips `publish = true`, finalises
metadata, ensures the LICENSE file is the full Apache-2.0 text, and
verifies every member with `cargo publish --dry-run`. It does not
publish anything. The first real `cargo publish` waits on a tagged
release of 0.5.0 (or later) once the workspace is ready for a public
audience.
