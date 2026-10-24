{
  mkNixpkgsBinary,
  packageSetFor,
  portableSystems,
  goDarwin,
  ...
}: let
  microPackageFor = target: targetPkgs: let
    package =
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "micro";
          subPackage = "./cmd/micro";
        }
      else (packageSetFor target targetPkgs).micro;
  in
    assert package.version == "2.0.15";
      package.overrideAttrs (old: {
        outputs = ["out"];
        nativeBuildInputs = builtins.filter (
          input: (input.name or "") != "install-shell-files"
        ) (old.nativeBuildInputs or []);
        env =
          (old.env or {})
          // {
            CGO_ENABLED = 0;
            GO_EXTLINK_ENABLED = 0;
            GOFLAGS = "${old.env.GOFLAGS or ""} -buildmode=exe";
          };
        ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
        postConfigure =
          (old.postConfigure or "")
          + ''
            # Keep the pinned module hash when changing plugin defaults.
            patch -p1 < ${../../patches/micro-disable-default-linter.patch}
          '';
        postInstall = "";
        doInstallCheck = false;
        nativeInstallCheckInputs = [];
      });
in
  mkNixpkgsBinary {
    name = "micro";
    synopsis = "Edit text in a terminal with syntax highlighting and familiar key bindings";
    systems = portableSystems;
    packageFor = microPackageFor;
  }
