{
  mkNixpkgsBinary,
  buildPkgs,
  linuxSystems,
  portableSystems,
  staticSetFor,
  packageSetFor,
  goDarwin,
  targetStrip,
  axePortableCaBundle,
  ...
}: let
  resticPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "restic";
          subPackage = "./cmd/restic";
        }
      else (packageSetFor target targetPkgs).restic
    ).overrideAttrs (old: {
      outputs = ["out"];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      patches = (old.patches or []) ++ [../patches/restic-embedded-ca.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} cmd/restic/axe-ca-bundle.pem
        '';
      postInstall = "";
    });
  fioMinimal = staticSet: let
    canExecute = staticSet.stdenv.buildPlatform.canExecute staticSet.stdenv.hostPlatform;
  in
    (staticSet.fio.override {
      withGnuplot = false;
      withLibnbd = false;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      buildInputs =
        [
          staticSet.libaio
          staticSet.zlib
        ]
        ++ buildPkgs.lib.optional canExecute staticSet.cunit;
      nativeBuildInputs = [staticSet.buildPackages.pkg-config];
      pythonPath = [];
      propagatedBuildInputs = [];
      dontAddPrefix = true;
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--build-static"
        ];
      postPatch = "";
      buildFlags =
        ["fio"]
        ++ buildPkgs.lib.optional canExecute "unittests/unittest";
      doCheck = canExecute;
      installPhase = ''
        runHook preInstall
        install -Dm755 fio "$out/bin/fio"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
    });
  rsyncDarwin = staticSet:
    staticSet.rsync.overrideAttrs (_: {
      outputs = ["out"];
      preBuild = "";
      doCheck = false;
      nativeCheckInputs = [];
      doInstallCheck = false;
      installPhase = ''
        runHook preInstall
        install -Dm755 rsync "$out/bin/rsync"
        ${targetStrip staticSet} -s "$out/bin/rsync"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
    });

  s5cmdPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "s5cmd";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).s5cmd
    ).overrideAttrs (old: {
      outputs = ["out"];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      patches = (old.patches or []) ++ [../patches/s5cmd-embedded-ca.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} axe-ca-bundle.pem
        '';
    });

  rclonePackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "rclone";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).rclone.override {enableCmount = false;}
    ).overrideAttrs
    (old: {
      outputs = ["out"];
      buildInputs = [];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      tags = ["noselfupdate"];
      patches = (old.patches or []) ++ [../patches/rclone-axe-defaults.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} axe-ca-bundle.pem
        '';
      postConfigure = ''
        rm -r cmd/gui cmd/selfupdate cmd/selfupdate_enabled.go \
          cmd/selfupdate_disabled.go fs/rc/webgui
        export GOFLAGS="$GOFLAGS -tags=noselfupdate"
      '';
      postInstall = "";
      postFixup = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });
in {
  restic = mkNixpkgsBinary {
    name = "restic";
    synopsis = "Back up and restore files in encrypted repositories";
    systems = portableSystems;
    packageFor = resticPackageFor;
  };
  fio = mkNixpkgsBinary {
    name = "fio";
    synopsis = "Benchmark and verify storage I/O workloads";
    systems = linuxSystems;
    packageFor = target: targetPkgs: fioMinimal (staticSetFor target targetPkgs);
  };

  rsync = mkNixpkgsBinary {
    name = "rsync";
    synopsis = "Synchronize files efficiently between local and remote paths";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    packageFor = target: targetPkgs:
      if target == "aarch64-darwin"
      then rsyncDarwin (staticSetFor target targetPkgs)
      else (packageSetFor target targetPkgs).rsync;
    darwinSystemLibraries = [
      "libiconv.2.dylib"
      "libz.dylib"
    ];
  };

  rclone = mkNixpkgsBinary {
    name = "rclone";
    synopsis = "Copy and synchronize files with remote storage";
    systems = portableSystems;
    packageFor = rclonePackageFor;
  };

  s5cmd = mkNixpkgsBinary {
    name = "s5cmd";
    synopsis = "Copy and manage S3 objects with parallel batch operations";
    systems = portableSystems;
    packageFor = s5cmdPackageFor;
  };

  findmnt = mkNixpkgsBinary {
    name = "findmnt";
    synopsis = "Find and describe mounted filesystems";
    systems = linuxSystems;
    packageFor = system: pkgs: (staticSetFor system pkgs).util-linuxMinimal;
  };
}
