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
      postConfigure =
        (old.postConfigure or "")
        + ''
          # Keep the upstream vendor hash when removing downloader imports.
          patch -p1 < ${../../patches/katana-axe-defaults.patch}
          patch -p1 < ${../../patches/katana-embedded-ca.patch}
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
