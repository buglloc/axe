---
name: axe-store
description: Maintain the AXE Store package inventory and release metadata. Use when adding, updating, removing, building, or publishing AXE Store tools; changing Nix package sources; or adding and removing OS and architecture targets.
---

# AXE Store maintenance

Keep the root `flake.nix` and `flake.lock` as the entry point and pin set for the package graph. Keep package modules, package-only patches, and static inputs together under `store/nix/`; category definitions in `store/nix/packages/*.nix` are the package inventory source of truth. Treat `store/bootstrap.json` as generated output; never hand-edit it.

## Workflow

1. Enter the repository shell with `nix develop .#default`. Never use a `path:` flake reference.
2. Read the affected category file, `store/nix/packages/lib.nix`, and `store/nix/packages/default.nix`. For target changes, also read `axe_artifact::Target` and `axe-store::build_pipeline::nix_system`.
3. Choose the existing helper that matches the source and target shape. Do not add a second package schema:
   - `mkSingleBinary`: one Nix package on one target.
   - `mkNixpkgsBinary`: one nixpkgs package selected per target.
   - `mkNixpkgsPackage`: a nixpkgs package tree needed at runtime.
   - `mkUpstreamBinary`: one checksum-pinned upstream binary on one target.
   - `mkUpstreamBinaries`: checksum-pinned upstream binaries for multiple targets.
4. Edit the category definition. Package attributes must be globally unique; the AXE Store ID is generated as `<category>/<name>`.
   Keep package-only patches and static inputs in `store/nix/patches/` and `store/nix/assets/`; category modules must not reach outside the `store/nix/` subtree.
5. Make every new `.nix` file visible to the Git-backed flake source before evaluation. `git add --intent-to-add <file>` keeps its content unstaged while preventing a misleading “file not found” evaluation failure.
6. Build every declared target separately with a focused package build. A successful aggregate AXE Store build may still report a target as `unsupported`; inspect both the target count and every warning. Do not rebuild the full Index to validate one package.
7. Regenerate and verify `store/bootstrap.json` with `just store-bootstrap` and `just check-store-bootstrap`.
8. Update the `On-demand` rows in the supported-software table in `README.md` after every package addition, removal, rename, or synopsis change. Keep each description to one short sentence.
9. Exercise the changed package through `axe-store build`, not only through `nix build`. This validates executable format, architecture, portability, packaging, signatures, and manifest metadata.
10. Run `just store-nix-smoke` after package graph or target-handling changes. Run `just store-smoke` when AXE Store consumer or publication behavior is affected. Remove temporary producer outputs afterward.
11. Remote builders are opt-in per checkout via the gitignored `nix/builders.conf` plus `keys/nix/id_ed25519` and `keys/nix/known_hosts` (layout in BOOTSTRAP.md §7; `just keyscan-nix-builders` rescans host keys from the conf). Without the conf, AXE Store containers build purely locally; never assume a remote builder exists, and never commit these files.

Never use an explicit `path:` flake reference for this checkout. It bypasses Git filtering and can copy ignored build output such as `target/` into `/nix/store`. Use `.#…`, an ordinary filesystem path, or let `axe-store` construct the reference.

## Targets and portability

Use only canonical target names:

- `x86_64-linux`
- `aarch64-linux`
- `aarch64-darwin`

Do not infer a target from an upstream filename. Download or build it, inspect the executable format and architecture, then declare the target.

For Linux single binaries, prefer static outputs. Select them through `staticSetFor` or `packageSetFor` so `aarch64-linux` uses the repository's cross-musl set; verify that ELF has neither `INTERP` nor `NEEDED`. Do not equate a successful Nix build with static linkage.

For CGO-free Go tools, `goDarwin` cross-builds `aarch64-darwin` on the Linux builder. Other Darwin outputs are acceptable only when the exact derivation is available from a configured binary cache or a Darwin remote builder. Test the explicit Darwin flake output before adding the target; evaluation alone does not prove build availability.


## Adding and updating packages

Prefer nixpkgs over a manually mirrored binary when nixpkgs provides the correct implementation, version family, target coverage, and linkage. For nixpkgs packages, the stable AXE Store version comes from `package.version`; all target packages must resolve to the same version.

`mkNixpkgsBinary` exposes exactly `bin/<name>` as a single-output AXE Store artifact. Manpages, shell completions, development outputs, and documentation do not belong in a single-binary package. If the upstream nixpkgs derivation generates expensive extras, override its documentation/completion hooks too; do not merely copy the unwanted tree and rely on AXE Store packaging to hide it.

Use `mkNixpkgsPackage` only when the executable needs adjacent libraries or other runtime files that cannot be folded into a single portable executable. Set its `entrypoint` to the package-relative executable path and keep the output tree minimal.

Nixpkgs executables must not retain `/nix/store/` runtime references. The helper rewrites the standard Nix-patched Go paths for MIME types, protocols, services, and timezone data back to system paths, then rejects any remaining Store reference. If another package embeds references, prefer a source/build override with portable prefixes. Set `rewriteBuildConfigurationPaths = true` only after proving every remaining reference is confined to diagnostic build-configuration text and an uncached foreign build prevents the source override. Never make the validator accept a runtime reference.

For upstream binaries:

1. Pin an exact version and immutable URL.
2. Calculate each hash with `nix store prefetch-file --json <url>`.
3. Keep one URL and hash per target.
4. Inspect ELF, Mach-O, or PE architecture before declaring the target.
5. Smoke-run only the native artifact; use structural validation for foreign targets.

Read [references/package-recipes.md](references/package-recipes.md) for helper examples, target addition/removal, static checks, and the verification matrix.

## Removing packages, channels, and targets

Remove obsolete definitions cleanly; do not leave aliases, empty categories, compatibility attributes, or stale bootstrap entries. Regenerate bootstrap after removal.

Publication deliberately rejects removal of an already published package, channel, or target. Confirm that every removal listed by the rejected publication is intended, then pass `--allow-target-removal` only to that explicit publish or sync operation. Never add this flag to default recipes.

Do not publish unless the user explicitly requests publication and the configured credentials, signing key, and destination are known.