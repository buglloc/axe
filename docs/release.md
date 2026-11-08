# Local AXE releases

Publish AXE binaries and the GitHub Release from a trusted local OSS publisher, not CI. GitHub Actions deploys the website; operators install binaries on remote machines. For publisher identities and Store access, see [BOOTSTRAP.md](../BOOTSTRAP.md).

## Inputs and trust

Enter the development shell with `just shell`. Check that `AXE_EDITION_ROOT` is unset or points to the public checkout, `edition.json` identifies `oss`, `config/store.json` points to the intended bucket, and `gh` is authenticated to `buglloc/axe`.

The publisher needs its private Store signing key and S3 credentials. Public `store/trusted/*.pub` and the signed `store/bootstrap-index.cbor.zst` are tracked inputs. The binary embeds Store trust and the snapshot, never the signing key or S3 credentials. Keep secrets in ignored paths and out of logs or shared evidence. `release-publish` prefers `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` when set; unset them and `AWS_SESSION_TOKEN` to use `keys/store/s3_access_key_id` and `keys/store/s3_secret_access_key`.

If the inventory or snapshot needs updating, run `just store-sync` on the trusted publisher first. This publishes Store packages and updates the Index and local snapshot, not AXE binaries. Review and commit the inventory and snapshot together before the AXE release. Check the destination and [target-removal safeguards](store.md#publication) first. Release staging requires a signed snapshot; the build checks its trust and inventory.

## Workflow 1: prepare and freeze the source

From a clean `main`, choose the SemVer change:

```bash
just release-prepare patch  # or minor / major
```

For a first release with no `vX.Y.Z` tag, use the current workspace version:

```bash
just release-prepare initial
```

`release-prepare` drafts `release-notes/vX.Y.Z.md` with OMP and updates the workspace version and lockfile. Preparation may need registry access even when builds are cached. It does not create a tag or publish anything.

Review every claim against the code and commits. Correct or remove unsupported claims, then commit and push the reviewed source. This is the release boundary: do not add source commits after this push.

```bash
git add Cargo.toml Cargo.lock release-notes/
git commit -m 'Release vX.Y.Z'
git push origin main
```

## Workflow 2: validate and publish

Run the complete publisher workflow:

```bash
just release-publish
```

`release-publish` checks formatting, the project and Store inventory, Clippy, and smoke tests. It builds `axe`, `axe-relay`, and `vzik` for all three supported targets, checks the runnable x86_64 `axe` binary's version, commit, and `oss` edition, generates `registry.json`, and stages the complete release. Existing staged assets are reused rather than rebuilt.

The optional local preflight is still available when the publisher wants to test both Store publication phases without touching remote systems:

```bash
just release-check /path/outside/checkout/axe-release-check
```

Keep `dist/releases/vX.Y.Z/`, or `$AXE_RELEASE_DIR/releases/vX.Y.Z` when overridden. It holds the tested assets, checksums, reviewed notes, and release metadata needed for publication and retries. Use a directory outside the checkout, or an ignored path under `dist/`, for the local preflight.

`release-publish`:

1. Creates and pushes the annotated `vX.Y.Z` tag and opens a draft GitHub Release with the reviewed notes.
2. Uploads immutable S3 objects for the ordinary binaries, universal bootstrap assets (`b/sh`, `b/py`, `b/js`, `b/pl`) and Native loaders (`b/amd64-linux`, `b/aarch64-linux`), then downloads them without credentials and verifies their SHA-256. It also fetches each `axe` payload over plain HTTP, the bootstrap fallback transport, and fails on a redirect or a digest mismatch.
3. Uploads `bootstrap.json`, `bootstrap-manifest.json`, `registry.json`, and `SHA256SUMS` to GitHub. The bootstrap metadata contains the immutable URLs and mutable stable aliases for the S3 assets; `bootstrap.json` also embeds every script and Native loader so the website can build paste commands. Every GitHub asset is downloaded and verified before publishing the draft.
4. Verifies remote immutable objects before updating stable S3 paths.
5. Updates `nix/axe-releases.json`, `web/content/changelog.md`, and the README download table, then creates and pushes the metadata commit.

Publication is not atomic across GitHub, S3, and Git. If a remote step fails before the metadata commit, rerun `just release-publish` from the same source commit with the same staged bytes. Do not move the tag, rebuild with different inputs, replace immutable objects, or change the approved notes. Existing assets are checked on retry. A stable-path failure can leave targets on different versions, and GitHub may be public before stable paths or the metadata commit are updated.

If the process stops after creating the local metadata commit but before pushing it, rerun `just release-publish`; it resumes that push instead of rebuilding or republishing.

Check the published URLs, hashes, metadata, README table, and `/changelog/` after the workflow completes. The website workflow runs on main pushes and published releases. It uses that release's `registry.json` for release events and the latest release's registry for main pushes; without that asset, deployment is skipped.

This procedure accepts only the public OSS edition root. Other editions need their own publisher, destination, and release process.
