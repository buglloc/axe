# Local AXE releases

Publish AXE binaries and the GitHub Release from a trusted local OSS publisher. This does not use PRs or release CI, and it does not install binaries on remote machines. A separate GitHub Actions workflow deploys the website. For Store inventory, keys, and publisher setup, see [BOOTSTRAP.md](../BOOTSTRAP.md#axe-store-from-scratch).

## Inputs and trust

Enter `nix develop .#default`. Before publishing, check that `edition.json` identifies the `oss` edition, `config/store.json` points to the intended bucket, and `gh` is authenticated to `buglloc/axe`. The publisher also needs its edition-local private `keys/store/` identity. Public `store/trusted/*.pub` and the signed `store/bootstrap-index.cbor.zst` are tracked inputs. AXE embeds the public trust keys and signed bootstrap Index, but never the Store signing key or S3 credentials. Keep secrets in ignored edition-local paths, out of tracked files and OMP evidence.

If the package inventory or signed snapshot needs updating, run `just store-sync` on the trusted publisher first. It publishes Store objects and replaces the local bootstrap snapshot; review and commit the updated public snapshot and inventory together before the AXE release. It does not publish AXE binaries. Check the destination and target-removal safeguards in BOOTSTRAP.md before running it. An AXE release build rejects a missing snapshot or one that does not match the trusted keys and inventory.

## Prepare the reviewed source

From a clean `main`, use `initial` for the first release (there is no `vX.Y.Z` tag yet):

```bash
just release-prepare initial
```

For later releases, choose the SemVer change:

```bash
just release-prepare patch  # or minor / major
```

`release-prepare` uses the current workspace version for the first release; thereafter it calculates the next version from the last `vX.Y.Z` tag. It updates the workspace version and lockfile, then asks `omp -p` to draft `release-notes/vX.Y.Z.md` from public Git history and changed source. These notes are not in Hugo's content tree and do not appear on the website before publication. OMP does not choose the version or approve the release. Check every claim against the code and commits. Correct or remove unsupported claims, then commit the version, lockfile, and notes and push `main`:

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

`release-check` runs formatting, project checks, Clippy, smoke tests, and builds AXE and `axe-relay` for all three supported targets. It checks the runnable x86_64 binary's version, commit, and `oss` edition, generates the website command inventory `registry.json` from that staged binary, then exercises both publisher phases against a local directory. Use a directory outside the checkout, or an ignored path under `dist/`, so the generated objects do not dirty the worktree. The command neither creates a tag nor uploads anything. Keep `dist/releases/vX.Y.Z/`: it contains the tested assets, `registry.json`, `SHA256SUMS`, reviewed notes, and snapshot digest needed for publication and retries.

`release-publish` requires that commit on `origin/main` and refuses staged bytes from another commit or snapshot. It then:

1. Creates and pushes the annotated `vX.Y.Z` tag and opens a draft GitHub Release with the reviewed notes.
2. Uploads immutable S3 objects with `axe-store release --immutable-only`, downloads them without credentials, and checks their SHA-256 against the staged AXE binaries.
3. Uploads three `axe` binaries, three `axe-relay` binaries, `registry.json`, and `SHA256SUMS` to GitHub. It downloads and verifies every asset before publishing the draft.
4. Runs `axe-store release --stable-only`. This checks the staged metadata and every remote immutable AXE object before updating stable paths. The publisher then downloads the stable objects without credentials, verifies their SHA-256, writes `nix/axe-releases.json`, adds the reviewed notes to `web/content/changelog.md`, and fills in the README download table.

Check the published URLs, hashes, metadata, README table, and `/changelog/`. Review those post-publication changes, commit them, and push `main`; the website workflow downloads `registry.json` from the latest GitHub Release and deploys the site with separate credentials. It does not build or publish release binaries.

GitHub, S3, Git, and the website cannot be published atomically. If a step fails, retry from the same commit with the same `dist/releases/vX.Y.Z/` bytes. Do not move the tag, rebuild it with different inputs, replace immutable objects, or silently change the approved notes. The publisher checks existing assets on retry. A failure while updating stable paths can leave targets on different versions; retry with the same staged release. The GitHub Release may be public before all stable paths or the README have been updated.

Other editions need their own publisher and bucket. This GitHub workflow accepts only the OSS edition root and does not use material from an external distribution.
