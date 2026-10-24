{
  mkNixpkgsBinary,
  packageSetFor,
  linuxSystems,
  ...
}: let
  superfilePackageFor = target: targetPkgs: let
    package = (packageSetFor target targetPkgs).superfile;
  in
    assert package.version == "1.6.0";
      package.overrideAttrs (old: {
        outputs = ["out"];
        subPackages = ["."];
        env =
          (old.env or {})
          // {
            CGO_ENABLED = 0;
            GO_EXTLINK_ENABLED = 0;
          };
        ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
        postConfigure =
          (old.postConfigure or "")
          + ''
            # Keep the pinned vendor hash while changing startup and config loading.
            patch -p1 < ${../../patches/superfile-portable-defaults.patch}
          '';
        postInstall = ''
          mv "$out/bin/superfile" "$out/bin/spf"
        '';
        doCheck = target == "x86_64-linux";
        checkPhase = ''
          runHook preCheck
          export GOFLAGS=''${GOFLAGS//-trimpath/}
          go test ./src/pkg/utils ./src/internal/common ./src/internal/ui/sidebar ./src/pkg/file_preview
          runHook postCheck
        '';
        doInstallCheck = false;
        meta = (old.meta or {}) // {mainProgram = "spf";};
      });
in
  mkNixpkgsBinary {
    name = "spf";
    synopsis = "Browse and manage files in a terminal interface with built-in previews";
    systems = linuxSystems;
    packageFor = superfilePackageFor;
  }
