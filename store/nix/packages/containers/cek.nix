{
  mkNixpkgsBinary,
  buildPkgs,
  packageSetFor,
  portableSystems,
  axePortableCaBundle,
  ...
}: let
  version = "0.3.0";
  cekPackage = packageSet:
    packageSet.buildGoModule {
      pname = "cek";
      inherit version;
      src = buildPkgs.fetchFromGitHub {
        owner = "bschaatsbergen";
        repo = "cek";
        rev = "b4af525f7a3f3f2ffd877c43c91f6cd9579cbccb";
        hash = "sha256-VogGUyi7fTQS4srKA6anvsJF7OgwuaVZKGFLpYtZItw=";
      };
      vendorHash = "sha256-cgodCRRdyVSyz5uigkvMIvfM+/qjiDe6AdP1AvDa+Jc=";
      patches = [../../patches/cek-embedded-ca.patch];
      postPatch = ''
        cp ${axePortableCaBundle} axe-ca-bundle.pem
      '';
      env.CGO_ENABLED = 0;
      subPackages = ["."];
      tags = ["timetzdata"];
      ldflags = [
        "-s"
        "-w"
        "-linkmode=internal"
        "-X github.com/bschaatsbergen/cek/version.Version=v${version}"
      ];
      # Command and OCI integration tests require live registries or a daemon.
      checkPhase = ''
        runHook preCheck
        go test -tags=timetzdata ./internal/overlay ./internal/view
        runHook postCheck
      '';
    };

  cekDarwin = (cekPackage buildPkgs).overrideAttrs (old: {
    env =
      old.env
      // {
        GOOS = "darwin";
        GOARCH = "arm64";
      };
    buildPhase = ''
      runHook preBuild
      export GOCACHE="$TMPDIR/go-cache"
      export GOTOOLCHAIN=local
      go build -buildmode=exe -trimpath -tags=timetzdata \
        -ldflags "${toString old.ldflags}" \
        -o "$TMPDIR/cek" .
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      install -Dm755 "$TMPDIR/cek" "$out/bin/cek"
      runHook postInstall
    '';
    dontStrip = true;
    doCheck = false;
    doInstallCheck = false;
  });
in
  mkNixpkgsBinary {
    name = "cek";
    synopsis = "Inspect, copy, and compare files in OCI images without a container runtime";
    systems = portableSystems;
    packageFor = target: targetPkgs:
      if target == "aarch64-darwin"
      then cekDarwin
      else cekPackage (packageSetFor target targetPkgs);
  }
