{
  mkNixpkgsBinary,
  buildPkgs,
  packageSetFor,
  portableSystems,
  goDarwin,
  ...
}: let
  zgrab2PackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "zgrab2";
          subPackage = "./cmd/zgrab2";
        }
      else (packageSetFor target targetPkgs).zgrab2
    ).overrideAttrs (old: {
      outputs = ["out"];
      env =
        (old.env or {})
        // {
          CGO_ENABLED = 0;
          GOFLAGS = "${old.env.GOFLAGS or ""} -buildmode=exe";
        };
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      postInstall = "";
      postConfigure =
        (old.postConfigure or "")
        + ''
          patch -p1 < ${../../patches/zgrab2-embedded-blocklist.patch}
        ''
        + buildPkgs.lib.optionalString (target == "aarch64-darwin") ''
          chmod u+w vendor/github.com/zmap/zcrypto/ct/x509 \
            vendor/github.com/zmap/zcrypto/ct/x509/root_nocgo_darwin.go
          patch -p1 < ${../../patches/zcrypto-darwin-arm64-nocgo.patch}
        '';
    });
in
  mkNixpkgsBinary {
    name = "zgrab2";
    synopsis = "Scan application-layer protocols and collect service metadata";
    systems = portableSystems;
    packageFor = zgrab2PackageFor;
  }
