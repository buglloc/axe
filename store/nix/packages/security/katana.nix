{
  mkNixpkgsBinary,
  axePortableCaBundle,
  packageSetFor,
  portableSystems,
  goDarwin,
  ...
}: let
  katanaPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "katana";
          subPackage = "./cmd/katana";
        }
      else (packageSetFor target targetPkgs).katana
    ).overrideAttrs (old: {
      outputs = ["out"];
      env =
        (old.env or {})
        // {
          CGO_ENABLED = 0;
          GOFLAGS = "${old.env.GOFLAGS or ""} -buildmode=exe";
        };
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      patches =
        (old.patches or [])
        ++ [
          ../../patches/katana-axe-defaults.patch
          ../../patches/katana-embedded-ca.patch
        ];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} cmd/katana/axe-ca-bundle.pem
        '';
    });
in
  mkNixpkgsBinary {
    name = "katana";
    synopsis = "Crawl web applications and discover URLs and endpoints";
    systems = portableSystems;
    packageFor = katanaPackageFor;
  }
