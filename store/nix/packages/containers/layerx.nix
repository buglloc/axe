{
  mkNixpkgsBinary,
  buildPkgs,
  portableSystems,
  packageSetFor,
  ...
}: let
  version = "1.6.2";
  commit = "d240566762a098e570bf4fc670177e7436999e6c";
  src = buildPkgs.fetchFromGitHub {
    owner = "deveshctl";
    repo = "layerx";
    tag = "v${version}";
    hash = "sha256-7E27ld6nrfJFLjsWF/LwaR224nyQysH+h0Z3gBkxH4g=";
  };

  packageFor = system: pkgs: let
    isDarwin = system == "aarch64-darwin";
    packageSet =
      if isDarwin
      then buildPkgs
      else packageSetFor system pkgs;
    package = packageSet.buildGoModule {
      pname = "layerx";
      inherit version src;
      vendorHash = "sha256-xTq1p7/0puYIGgTX4eCsDwLNdN+6KqAsNLWqeOlWqXE=";
      subPackages = ["."];
      env.CGO_ENABLED = 0;
      ldflags = [
        "-s"
        "-w"
        "-linkmode=internal"
        "-X main.version=${version}"
        "-X main.commit=${commit}"
        "-X main.date=2026-10-07T07:14:45Z"
      ];
      doCheck = system == "x86_64-linux";
      checkPhase = ''
        runHook preCheck
        export GOFLAGS="''${GOFLAGS//-trimpath/}"
        go test ./...
        runHook postCheck
      '';
      meta = {
        description = "Terminal container image layer inspector";
        homepage = "https://github.com/deveshctl/layerx";
        license = buildPkgs.lib.licenses.mit;
        mainProgram = "layerx";
        platforms = portableSystems;
      };
    };
  in
    if isDarwin
    then
      package.overrideAttrs (old: {
        env =
          old.env
          // {
            GOOS = "darwin";
            GOARCH = "arm64";
          };
        buildPhase = ''
          runHook preBuild
          go build -buildmode=exe -trimpath \
            -ldflags "${toString old.ldflags}" \
            -o "$TMPDIR/layerx" .
          runHook postBuild
        '';
        installPhase = ''
          runHook preInstall
          install -Dm755 "$TMPDIR/layerx" "$out/bin/layerx"
          runHook postInstall
        '';
        dontStrip = true;
      })
    else package;
in
  mkNixpkgsBinary {
    name = "layerx";
    synopsis = "Inspect container image layers in a terminal UI or CI";
    systems = portableSystems;
    inherit packageFor;
  }
