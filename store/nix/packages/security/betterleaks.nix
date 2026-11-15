{
  mkNixpkgsBinary,
  buildPkgs,
  packageSetFor,
  portableSystems,
  goDarwin,
  axePortableCaBundle,
  ...
}: let
  version = "1.10.1";
  src = buildPkgs.fetchFromGitHub {
    owner = "betterleaks";
    repo = "betterleaks";
    rev = "39d5bdcf34db54e944facac5cf6e8b03db5e2b99";
    hash = "sha256-27u8JVp6KRfY9ZFTdN7dSooW4nt6dCDhfjFSU3YKajE=";
  };
  betterleaksPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "betterleaks";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).betterleaks
    ).overrideAttrs (old:
      {
        inherit version src;
        vendorHash = "sha256-ro62XSWPEXLWxbs2iLZahlcZOaEYCLUZerat0PPtg34=";
        outputs = ["out"];
        patches = (old.patches or []) ++ [../../patches/betterleaks-embedded-ca.patch];
        postPatch =
          (old.postPatch or "")
          + ''
            cp ${axePortableCaBundle} cmd/axe-ca-bundle.pem
          '';
        env =
          (old.env or {})
          // {
            CGO_ENABLED = 0;
            GOFLAGS = "${old.env.GOFLAGS or ""} -buildmode=exe";
          };
        ldflags = [
          "-s"
          "-w"
          "-linkmode=internal"
          "-X github.com/betterleaks/betterleaks/version.Version=${version}"
        ];
        nativeCheckInputs = (old.nativeCheckInputs or []) ++ [buildPkgs.git];
        postInstall = "";
        meta =
          (old.meta or {})
          // {
            changelog = "https://github.com/betterleaks/betterleaks/releases/tag/v${version}";
          };
      }
      // buildPkgs.lib.optionalAttrs (target == "aarch64-darwin") {
        # goDarwin captures nixpkgs ldflags before the release version override.
        buildPhase = ''
          runHook preBuild
          export GOCACHE="$TMPDIR/go-cache"
          export GOTOOLCHAIN=local
          go build -buildmode=exe -trimpath \
            -ldflags "-s -w -linkmode=internal -X github.com/betterleaks/betterleaks/version.Version=${version}" \
            -o "$TMPDIR/betterleaks" .
          runHook postBuild
        '';
      });
in
  mkNixpkgsBinary {
    name = "betterleaks";
    synopsis = "Scan files, stdin, and Git history for secrets with optional validation";
    systems = portableSystems;
    packageFor = betterleaksPackageFor;
  }
