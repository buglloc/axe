{
  mkNixpkgsBinary,
  buildPkgs,
  portableSystems,
  staticSetFor,
  ...
}: let
  sevenZipMinimal = staticSet:
    (staticSet._7zz.override {
      useAsmc = false;
      useUasm = false;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      setupHook = null;
      makeFlags = (old.makeFlags or []) ++ ["LDFLAGS_STATIC_2=-static"];
      postBuild = "";
      installPhase = ''
        runHook preInstall
        install -Dm755 b/*/7zz "$out/bin/7zz"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doCheck = false;
      doInstallCheck = false;
    });
  sevenZipDarwin = let
    version = buildPkgs._7zz.version;
    archive = buildPkgs.fetchurl {
      url = "https://github.com/ip7z/7zip/releases/download/${version}/7z${builtins.replaceStrings ["."] [""] version}-mac.tar.xz";
      hash = "sha256-HPZ2BXlQL4flkf9cc6AF7FCz5Nb1B+iwODgtVjwxdbk=";
    };
  in
    buildPkgs.runCommand "7zz-${version}-aarch64-darwin"
    {
      inherit version;
      nativeBuildInputs = [
        buildPkgs.gnutar
        buildPkgs.xz
      ];
    }
    ''
      tar -xJf ${archive} 7zz
      install -Dm755 7zz "$out/bin/7zz"
    '';
in {
  "7zz" = mkNixpkgsBinary {
    name = "7zz";
    synopsis = "Create and extract archives with 7-Zip";
    systems = portableSystems;
    packageFor = target: targetPkgs:
      if target == "aarch64-darwin"
      then sevenZipDarwin
      else sevenZipMinimal (staticSetFor target targetPkgs);
  };
}
