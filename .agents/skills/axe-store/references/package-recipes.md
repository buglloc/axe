# AXE Store package recipes

## Contents

- [Target matrix](#target-matrix)
- [Nixpkgs packages](#nixpkgs-packages)
- [Pinned upstream binaries](#pinned-upstream-binaries)
- [Adding a package](#adding-a-package)
- [Updating a package](#updating-a-package)
- [Removing packages and targets](#removing-packages-and-targets)
- [Verification matrix](#verification-matrix)
- [Publishing](#publishing)

## Target matrix

| AXE Store target | Executable | Nix output key | Notes |
|---|---|---|---|
| `x86_64-linux` | ELF x86-64 | `packages.x86_64-linux` | Prefer static single binaries. |
| `aarch64-linux` | ELF AArch64 | `packages.aarch64-linux` | Prefer static single binaries; structural checks can run on x86-64. |
| `aarch64-darwin` | Mach-O arm64 | `packages.aarch64-darwin` | CGO-free Go tools can use `goDarwin`; other packages need a cache hit or Darwin builder when invoked from Linux. |

The Rust `Target` enum and `nix_system` mapping are authoritative. Update both before introducing a genuinely new target string. Then update target parsing, executable validation, client platform detection, tests, package helpers, and this table as one clean cutover.

## Nixpkgs packages

Use `mkSingleBinary` for one target:

```nix
{ mkSingleBinary, pkgsFor, ... }:
let
  system = "x86_64-linux";
in
{
  tool = mkSingleBinary {
    name = "tool";
    synopsis = "Short user-facing description";
    inherit system;
    package = (pkgsFor system).pkgsStatic.tool;
  };
}
```

Use `mkNixpkgsBinary` for multiple targets. Start from the repository's target sets and package-set selectors instead of open-coding cross-package paths:

```nix
{
  mkNixpkgsBinary,
  portableSystems,
  packageSetFor,
  ...
}: {
  tool = mkNixpkgsBinary {
    name = "tool";
    synopsis = "Short user-facing description";
    systems = portableSystems;
    packageFor = system: pkgs: (packageSetFor system pkgs).tool;
  };
}
```

`packageSetFor` selects the cross-musl static set for `aarch64-linux`, the native static set for `x86_64-linux`, and the regular package set for Darwin. Use `staticSetFor` when a Darwin derivation needs only selected static libraries. Use `goDarwin` for an explicitly verified CGO-free Go program that should cross-build on the Linux builder. Import `pkgsFor` only for a package-specific selection that the shared selectors cannot express.

`mkNixpkgsBinary` derives the stable version from each package and rejects an empty target list or version disagreement. Do not hard-code a second version beside `package.version`.

Confirm the nixpkgs attribute identifies the intended program. Similar names may refer to different implementations; for example, a Go implementation can use an attribute such as `tool-go` while still installing `bin/tool`.

`mkNixpkgsBinary` normalizes each selected derivation to a single output containing only `bin/<name>`. It deliberately excludes manpages, completions, headers, libraries, and documentation from the AXE Store artifact. Avoid generating those extras when the nixpkgs recipe makes that practical:

```nix
minimalTool =
  package:
  package.overrideAttrs {
    configureFlags = [
      "--prefix=/usr"
      "--disable-docs"
    ];
    doCheck = false;
    doInstallCheck = false;
    installPhase = ''
      runHook preInstall
      install -Dm755 tool "$out/bin/tool"
      runHook postInstall
    '';
    postFixup = "";
  };
```

Use only flags supported by that package. A portable prefix such as `/usr` is required when configure arguments are compiled into the executable; `$out` would embed a `/nix/store/` reference. Preserve package checks unless the AXE Store producer performs the same relevant validation or the disabled check only covers outputs deliberately omitted from the artifact.

Nix patches the Go runtime to find MIME types, protocols, services, and timezone data in `/nix/store`. `mkNixpkgsBinary` replaces those known paths with their standard system locations without changing binary size, then fails if any Store reference remains. A remaining reference normally needs a package-specific source or build override. If inspection proves that every remaining reference belongs only to diagnostic build-configuration text and a foreign target is available solely as an immutable cache result, set `rewriteBuildConfigurationPaths = true`; the helper performs an equal-length diagnostic-prefix rewrite. Never use that escape hatch for a runtime path.

Use `mkNixpkgsPackage` when runtime behavior requires an adjacent library or data tree:

```nix
{ mkNixpkgsPackage, ... }:
{
  tool = mkNixpkgsPackage {
    name = "tool";
    synopsis = "Short user-facing description";
    systems = [ "x86_64-linux" ];
    entrypoint = "bin/tool";
    packageFor = _: pkgs: pkgs.tool;
  };
}
```

Keep the package tree minimal. Do not use a package artifact merely to avoid making a single executable portable.

## Pinned upstream binaries

For one target, use `mkUpstreamBinary` with exact `version`, `system`, `url`, and `hash`.

For multiple targets:

```nix
{ mkUpstreamBinaries, ... }:
{
  tool = mkUpstreamBinaries {
    name = "tool";
    version = "1.2.3";
    synopsis = "Short user-facing description";
    sources = {
      aarch64-darwin = {
        url = "https://example.invalid/tool_1.2.3_darwin_arm64";
        hash = "sha256-…";
      };
      aarch64-linux = {
        url = "https://example.invalid/tool_1.2.3_linux_arm64";
        hash = "sha256-…";
      };
      x86_64-linux = {
        url = "https://example.invalid/tool_1.2.3_linux_x86_64";
        hash = "sha256-…";
      };
    };
  };
}
```

Calculate hashes independently:

```bash
nix store prefetch-file --json https://example.invalid/tool
```

The returned SRI `hash` belongs to that exact URL. Never reuse a hash across platforms without byte-for-byte proof.

## Adding a package

1. Choose the category and attribute name.
2. Add the definition to `store/nix/packages/<category>.nix`.
3. If the category is new, import it once from `store/nix/packages/default.nix`.
   New category files are absent from Git-backed flake evaluation until Git knows about them; use `git add --intent-to-add store/nix/packages/<category>.nix` before the first Nix command.
4. Build every target output explicitly.
5. Inspect each executable format and architecture.
6. Regenerate bootstrap.
7. Build the package through `axe-store` into a temporary output.
8. Run the AXE Store Nix smoke test.

`artifact.path` for a single binary must match the installed path under the Nix output, normally `bin/<name>`. Use the package artifact form only when the tool needs a tree of runtime files.

## Updating a package

For nixpkgs-backed tools, update `flake.lock` only when the requested package update requires a newer pinned nixpkgs revision. Review unrelated package version changes before accepting the lock update.

For upstream tools, update version, every affected URL, and every hash together. Reject partial platform releases unless the target reduction is deliberate.

After either update, compare generated metadata for:

- exact stable version;
- canonical package ID;
- aliases;
- artifact kind and path;
- complete target set.

## Removing packages and targets

To remove one target, delete only its source/package mapping and rebuild the remaining targets. To remove a package, delete its attribute; remove an empty category import rather than retaining an empty file solely for compatibility.

Then:

1. Run `just store-bootstrap`.
2. Confirm the package or target disappeared from `store/bootstrap.json`.
3. Build a fresh AXE Store snapshot.
4. Compare its Index against the currently published Index.
5. Publish with `--allow-target-removal` only after the user explicitly confirms that removal.

Do not silently retain obsolete aliases or add placeholder derivations to evade the removal gate.

## Verification matrix

Build every declared Nix output:

```bash
nix build --no-link --print-out-paths .#packages.<target>.<attribute>
```

A Darwin command succeeding through substitution proves cache availability for that exact derivation. A local build plan or evaluation alone does not.

For Linux outputs, inspect the actual executable:

```bash
file /nix/store/<output>/bin/<tool>
readelf -l /nix/store/<output>/bin/<tool>
readelf -d /nix/store/<output>/bin/<tool>
```

For a required static binary, `readelf -l` must have no `INTERP`, `readelf -d` must have no `NEEDED`, and the binary must contain no `/nix/store/` runtime reference. Smoke-run the native binary with `--help` or a harmless real operation.

Build through the producer pipeline one target at a time. Give each build its own temporary output because the command replaces that directory:

```bash
just generate-dev-keys
output=$(mktemp -d)
trap 'rm -rf "$output"' EXIT
cargo run --quiet -p axe-store -- build \
  --package <attribute> \
  --target <target> \
  --output "$output"
```

Read stderr. `axe-store` reports unsupported targets without necessarily failing the whole aggregate build. Verify the final summary target count and inspect the generated signed manifest when target membership is the change under test.

Finish formatting and metadata checks with:

```bash
just nix-fmt
just store-bootstrap
just check-store-bootstrap
nix flake check --no-build .
just store-nix-smoke
```

Never write `path:.` in these commands.

## Publishing

Use `just store-build` for the complete signed snapshot and `just store-publish` only with an intentional destination and valid production credentials. `just store-sync` combines them.

Before production publication:

```bash
just store-smoke
just store-nix-smoke
```

A package, channel, or target removal requires the CLI's `--allow-target-removal` flag. Treat the flag as a reviewed exception, not a retry mechanism.