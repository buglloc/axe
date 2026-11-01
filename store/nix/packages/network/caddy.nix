{
  mkNixpkgsBinary,
  buildPkgs,
  packageSetFor,
  portableSystems,
  axePortableCaBundle,
  goDarwin,
  ...
}: let
  caddyPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "caddy";
          subPackage = "./cmd/caddy";
        }
      else (packageSetFor target targetPkgs).caddy
    ).overrideAttrs (old: {
      outputs = ["out"];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      postInstall = "";
      postConfigure =
        ''
          # The nixpkgs recipe removes the upstream fallback file; restore the
          # local embedded-CA implementation instead.
          patch -p1 < ${../../patches/caddy-embedded-ca.patch}
          patch -p1 < ${../../patches/caddy-disable-self-update.patch}
          cp ${axePortableCaBundle} cmd/axe-ca-bundle.pem
        ''
        + buildPkgs.lib.optionalString (target == "aarch64-darwin") ''
          export GOFLAGS="$GOFLAGS -tags=${buildPkgs.lib.concatStringsSep "," old.tags}"
        '';
    });
in
  mkNixpkgsBinary {
    name = "caddy";
    synopsis = "Serve HTTP and reverse proxy traffic with automatic HTTPS";
    systems = portableSystems;
    packageFor = caddyPackageFor;
  }
