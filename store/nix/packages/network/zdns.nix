{
  mkNixpkgsBinary,
  buildPkgs,
  packageSetFor,
  portableSystems,
  goDarwin,
  ...
}: let
  zdnsPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "zdns";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).zdns
    ).overrideAttrs (old: {
      outputs = ["out"];
      subPackages = ["."];
      env =
        (old.env or {})
        // {
          CGO_ENABLED = 0;
          GOFLAGS = (old.env.GOFLAGS or "") + " -buildmode=exe";
        };
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      postInstall = "";
      postConfigure =
        (old.postConfigure or "")
        + buildPkgs.lib.optionalString (target == "aarch64-darwin") ''
          chmod u+w vendor/github.com/zmap/zcrypto/ct/x509 \
            vendor/github.com/zmap/zcrypto/ct/x509/root_nocgo_darwin.go
          patch -p1 < ${../../patches/zcrypto-darwin-arm64-nocgo.patch}
        '';
    });
in
  mkNixpkgsBinary {
    name = "zdns";
    synopsis = "Perform high-speed DNS lookups with optional local recursive resolution";
    systems = portableSystems;
    packageFor = zdnsPackageFor;
  }
