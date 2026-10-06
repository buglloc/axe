# Local AXE releases

Publish AXE binaries and the GitHub Release from a trusted local OSS publisher, not CI. GitHub Actions deploys the website; operators install binaries on remote machines. For publisher identities and Store access, see [BOOTSTRAP.md](../BOOTSTRAP.md).

## Inputs and trust

Enter `nix develop .#default`. Check that `AXE_EDITION_ROOT` is unset or points to the public checkout, `edition.json` identifies `oss`, `config/store.json` points to the intended bucket, and `gh` is authenticated to `buglloc/axe`.

The publisher needs its private Store signing key and S3 credentials. Public `store/trusted/*.pub` and the signed `store/bootstrap-index.cbor.zst` are tracked inputs. The binary embeds Store trust and the snapshot, never the signing key or S3 credentials. Keep secrets in ignored paths and out of logs or shared evidence. `release-publish` prefers `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` when set; unset them and `AWS_SESSION_TOKEN` to use `keys/store/s3_access_key_id` and `keys/store/s3_secret_access_key`.

If the inventory or snapshot needs updating, run `just store-sync` on the trusted publisher first. This publishes Store packages and updates the Index and local snapshot, not AXE binaries. Review and commit the inventory and snapshot together before the AXE release. Check the destination and [target-removal safeguards](store.md#publication) first. Release staging requires a signed snapshot; the build checks its trust and inventory.

## Prepare the reviewed source

From a clean `main`, choose the SemVer change:

```bash
just release-prepare patch  # or minor / major
```

For a first release with no `vX.Y.Z` tag, use the current workspace version:

```bash
just release-prepare initial
```

`release-prepare` drafts `release-notes/vX.Y.Z.md` with OMP and updates the workspace version and lockfile. Preparation may need registry access even when builds are cached. It does not create a tag or publish the notes to the website.

Review every claim against the code and commits. Correct or remove unsupported claims, then commit the version, lockfile, and notes and push `main`:

```bash
git add Cargo.toml Cargo.lock release-notes/
git commit -m 'Release vX.Y.Z'
git push origin main
```

## Validate and publish

Use the same commit and signed snapshot for the check and publication:

```bash
just release-check /path/outside/checkout/axe-release-check
just release-publish
```

`release-check` checks formatting, the project and Store inventory, Clippy, and smoke tests. It builds `axe`, `axe-relay`, and `vzik` for all three supported targets and checks the runnable x86_64 `axe` binary's version, commit, and `oss` edition. It generates `registry.json` and exercises immutable and stable publication against the local directory without creating a tag or uploading to GitHub or S3. Use a directory outside the checkout, or an ignored path under `dist/`, to keep the worktree clean.

Keep `dist/releases/vX.Y.Z/`, or `$AXE_RELEASE_DIR/releases/vX.Y.Z/` when overridden. It holds the tested assets, checksums, reviewed notes, and release metadata needed for publication and retries. Existing staged assets are reused rather than rebuilt.

`release-publish` requires the reviewed commit on `origin/main` and the same staged assets, snapshot, and notes. It:

1. Creates and pushes the annotated `vX.Y.Z` tag and opens a draft GitHub Release with the reviewed notes.
2. Uploads immutable S3 objects for all nine binaries (`axe`, `axe-relay`, and `vzik` on three targets), then downloads them without credentials and verifies their SHA-256.
3. Uploads the binaries, `registry.json`, and `SHA256SUMS` to GitHub. It downloads and verifies every asset before publishing the draft.
4. Verifies the remote immutable objects before updating stable S3 paths, then downloads and checks the stable objects without credentials. It updates `nix/axe-releases.json` for `axe`, adds the reviewed notes to `web/content/changelog.md`, and fills the README download table with GitHub and S3 links for all nine binaries.

Check the published URLs, hashes, metadata, README table, and `/changelog/`. Review the generated changes, commit them, and push `main`. The website workflow runs on main pushes and published releases. It uses that release's `registry.json` for release events and the latest release's registry for main pushes; without that asset, deployment is skipped.

Publication is not atomic across GitHub, S3, Git, and the website. If a step fails, retry from the same commit with the same staged bytes. Do not move the tag, rebuild with different inputs, replace immutable objects, or change the approved notes. Existing assets are checked on retry. A stable-path failure can leave targets on different versions, and GitHub may be public before stable paths or the README are updated.

This procedure accepts only the public OSS edition root. Other editions need their own publisher, destination, and release process.
