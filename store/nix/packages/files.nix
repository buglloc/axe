{
  mkNixpkgsBinary,
  mkSingleBinary,
  pkgsFor,
  linuxSystems,
  portableSystems,
  packageSetFor,
  staticSetFor,
  ...
} @ helpers: let
  system = "x86_64-linux";
  buildPkgs = pkgsFor system;

  fdVersion = "10.5.0";
  fdSources = {
    aarch64-darwin = {
      url = "https://github.com/sharkdp/fd/releases/download/v${fdVersion}/fd-v${fdVersion}-aarch64-apple-darwin.tar.gz";
      hash = "sha256-tn4YNsRo5C5BGYS1blL6er7AjCvSLIZzmOfME0qsXhI=";
    };
    x86_64-linux = {
      url = "https://github.com/sharkdp/fd/releases/download/v${fdVersion}/fd-v${fdVersion}-x86_64-unknown-linux-musl.tar.gz";
      hash = "sha256-dhxy3I4SDYWyIpIGO+inluLusg6z5POLj6I0PM81FKc=";
    };
  };

  fdPackage = target: let
    source = fdSources.${target};
    archive = buildPkgs.fetchurl {
      inherit (source) url hash;
    };
  in
    buildPkgs.stdenvNoCC.mkDerivation {
      pname = "fd";
      version = fdVersion;
      inherit archive;
      dontUnpack = true;
      nativeBuildInputs = [buildPkgs.gnutar];
      installPhase = ''
        runHook preInstall
        tar -xzf "$archive" --strip-components=1
        install -Dm755 fd "$out/bin/fd"
        runHook postInstall
      '';
    };

  gixPackageFor = target: targetPkgs: let
    packageSet = packageSetFor target targetPkgs;
  in
    packageSet.gitoxide.overrideAttrs (old: {
      buildInputs = [];
      cargoBuildFlags = [
        "--bin"
        "gix"
        "--no-default-features"
        "--features"
        "max-pure"
      ];
      doCheck = false;
      doInstallCheck = false;
      installPhase = ''
        runHook preInstall
        install -Dm755 target/${packageSet.stdenv.hostPlatform.rust.rustcTarget}/release/gix "$out/bin/gix"
        runHook postInstall
      '';
      nativeInstallCheckInputs = [];
      preFixup = "";
      postFixup = "";
      postInstall = "";
    });

  gixVersion = "0.59.0";
  gixDarwinArchiveName = "gitoxide-max-pure-v${gixVersion}-aarch64-apple-darwin";
  gixDarwinArchive = buildPkgs.fetchurl {
    url = "https://github.com/GitoxideLabs/gitoxide/releases/download/v${gixVersion}/${gixDarwinArchiveName}.tar.gz";
    hash = "sha256-y6TVrRhbk8vuVNy4alWmgScry4/TieKcQm8ymCX7uMI=";
  };
  gixDarwinPackage = buildPkgs.stdenvNoCC.mkDerivation {
    pname = "gix";
    version = gixVersion;
    archive = gixDarwinArchive;
    dontUnpack = true;
    nativeBuildInputs = [buildPkgs.gnutar];
    installPhase = ''
      runHook preInstall
      tar -xzf "$archive" "${gixDarwinArchiveName}/gix" --strip-components=1
      install -Dm755 gix "$out/bin/gix"
      runHook postInstall
    '';
  };

  fdLinux = mkSingleBinary {
    name = "fd";
    synopsis = "Find filesystem entries by name and attributes";
    system = "x86_64-linux";
    package = fdPackage "x86_64-linux";
  };

  fdDarwin = mkSingleBinary {
    name = "fd";
    synopsis = "Find filesystem entries by name and attributes";
    system = "aarch64-darwin";
    package = fdPackage "aarch64-darwin";
  };
in {
  broot = import ./files/broot.nix helpers;
  spf = import ./files/superfile.nix helpers;

  fd =
    fdLinux
    // {
      targets = fdLinux.targets // fdDarwin.targets;
    };

  gix = let
    gixLinux = mkNixpkgsBinary {
      name = "gix";
      synopsis = "Inspect and manipulate Git repositories with a low-level Rust CLI";
      systems = linuxSystems;
      packageFor = gixPackageFor;
    };

    gixDarwin = mkSingleBinary {
      name = "gix";
      synopsis = "Inspect and manipulate Git repositories with a low-level Rust CLI";
      system = "aarch64-darwin";
      package = gixDarwinPackage;
    };
  in
    gixLinux
    // {
      targets = gixLinux.targets // gixDarwin.targets;
    };

  rg = mkNixpkgsBinary {
    name = "rg";
    synopsis = "Search file contents with regular expressions";
    systems = portableSystems;
    packageFor = target: pkgs: (staticSetFor target pkgs).ripgrep;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libiconv.2.dylib"];
  };

  fzf = mkNixpkgsBinary {
    name = "fzf";
    synopsis = "Filter and select items interactively with fuzzy matching";
    systems = portableSystems;
    packageFor = target: pkgs: (packageSetFor target pkgs).fzf;
  };
}
