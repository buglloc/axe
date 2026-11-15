{
  mkNixpkgsBinary,
  buildPkgs,
  packageSetFor,
  portableSystems,
  axePortableCaBundle,
  goDarwin,
  ...
}: let
  caddyPackageFor = target: targetPkgs: let
    packageSet =
      if target == "aarch64-darwin"
      then buildPkgs
      else packageSetFor target targetPkgs;
  in
    (
      (
        if target == "aarch64-darwin"
        then
          goDarwin {
            package = "caddy";
            subPackage = "./cmd/caddy";
          }
        else packageSet.caddy
      ).override {buildGoModule = packageSet.buildGoLatestModule;}
    ).overrideAttrs (old: {
      version = "2.11.9";
      src = buildPkgs.fetchFromGitHub {
        owner = "caddyserver";
        repo = "caddy";
        tag = "v2.11.9";
        hash = "sha256-K7irjePU998bcbKANXpTf0npirSh+tkc8D3x4JmRdbg=";
      };
      vendorHash = "sha256-qjLumZHCyoM+lNZtWOTmMlHN5zgB36fv9CkIq3ncMlM=";
      ldflags = ["-s" "-linkmode=internal" "-X github.com/caddyserver/caddy/v2.CustomVersion=v2.11.9"];
      outputs = ["out"];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      postInstall = "";
      buildPhase =
        if target == "aarch64-darwin"
        then ''
          runHook preBuild
          export GOCACHE="$TMPDIR/go-cache"
          export GOTOOLCHAIN=local
          go build -buildmode=exe -trimpath -ldflags "$ldflags" \
            -o "$TMPDIR/caddy" ./cmd/caddy
          runHook postBuild
        ''
        else old.buildPhase;
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
