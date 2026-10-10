{
  mkNixpkgsBinary,
  mkNixpkgsPackage,
  buildPkgs,
  axePortableCaBundle,
  linuxSystems,
  portableSystems,
  staticSetFor,
  packageSetFor,
  goDarwin,
  disableCdncheckIPv6Probe,
  ...
}: let
  capshMinimal = packageSet:
    (packageSet.libcap.override {runtimeShell = "/bin/sh";}).overrideAttrs (old: {
      patches = (old.patches or []) ++ [../patches/capsh-use-axe-shell.patch];
    });
  opensslMinimal = packageSet:
    (packageSet.openssl.override {static = true;}).overrideAttrs (old: {
      outputs = ["out"];
      separateDebugInfo = false;
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--openssldir=/etc/ssl"
        ];
      installPhase = ''
        runHook preInstall
        install -Dm755 apps/openssl "$out/bin/openssl"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doCheck = false;
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });
  binwalkMinimal = packageSet: compressionSet:
    (packageSet.binwalk.override {
      bzip2 = compressionSet.bzip2;
      python3 = {
        pkgs.python-lzo = null;
      };
      xz = compressionSet.xz;
    }).overrideAttrs
    (_: {
      buildInputs = [
        compressionSet.bzip2
        packageSet.dtc
        packageSet.fontconfig
        packageSet.lzo
        packageSet.openssl_3
        packageSet.ucl
        packageSet.unzip
        compressionSet.xz
        packageSet.zlib
      ];
      postInstall = "";
      doCheck = false;
      postFixup = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  # Binwalk links libbz2 and liblzma. Keep the native Darwin stdenv and make
  # only those runtime dependencies static.
  binwalkPackageFor = target: targetPkgs: let
    packageSet = packageSetFor target targetPkgs;
    compressionSet =
      if target == "aarch64-darwin"
      then staticSetFor target targetPkgs
      else packageSet;
  in
    binwalkMinimal packageSet compressionSet;

  agePackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "age";
          subPackage = "./cmd/age";
        }
      else (packageSetFor target targetPkgs).age
    ).overrideAttrs
    (old: {
      outputs = ["out"];
      subPackages = ["cmd/age"];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      preInstall = "";
    });

  gobusterPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "gobuster";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).gobuster
    ).overrideAttrs
    (old: {
      patches = (old.patches or []) ++ [../patches/gobuster-bundled-roots.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} libgobuster/axe-ca-bundle.pem
        '';
    });

  interactshClientPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "interactsh";
          subPackage = "./cmd/interactsh-client";
          binary = "interactsh-client";
        }
      else (packageSetFor target targetPkgs).interactsh
    ).overrideAttrs
    (old: {
      pname = "interactsh-client";
      subPackages = ["cmd/interactsh-client"];
      patches = (old.patches or []) ++ [../patches/interactsh-client-disable-update-check.patch];
    });

  nucleiTemplates = buildPkgs.nuclei-templates.overrideAttrs (old: {
    installPhase =
      (old.installPhase or "")
      + ''
        install -m 444 .nuclei-ignore .new-additions templates-checksum.txt \
          "$out/share/nuclei-templates/"
      '';
  });
  nucleiBinaryFor = target: targetPkgs:
    disableCdncheckIPv6Probe (
      (
        if target == "aarch64-darwin"
        then
          goDarwin {
            package = "nuclei";
            subPackage = "./cmd/nuclei";
          }
        else (packageSetFor target targetPkgs).nuclei
      ).overrideAttrs
      (old: {
        patches = (old.patches or []) ++ [../patches/nuclei-axe-defaults.patch];
        postPatch =
          (old.postPatch or "")
          + ''
            substituteInPlace pkg/catalog/config/nucleiconfig.go \
              --replace-fail '@nucleiTemplatesVersion@' 'v${nucleiTemplates.version}'
          '';
      })
    );
  nucleiPackageFor = target: targetPkgs: let
    binary = nucleiBinaryFor target targetPkgs;
  in
    buildPkgs.runCommand "nuclei-${binary.version}-templates-${nucleiTemplates.version}"
    {
      inherit (binary) version;
      nativeBuildInputs = [buildPkgs.perl];
    }
    ''
      install -Dm755 ${binary}/bin/nuclei "$out/bin/nuclei"
      mkdir -p "$out/share"
      cp -a ${nucleiTemplates}/share/nuclei-templates "$out/share/"
      perl -0pi -e 's{/nix/store/}{/usr/local/}g' "$out/bin/nuclei"
      if grep -R -a -Fq /nix/store/ "$out"; then
        echo "nuclei: packaged tree contains a /nix/store reference" >&2
        exit 1
      fi
    '';
in {
  age = mkNixpkgsBinary {
    name = "age";
    synopsis = "Encrypt and decrypt files with explicit keys or passphrases";
    systems = portableSystems;
    packageFor = agePackageFor;
  };

  openssl = mkNixpkgsBinary {
    name = "openssl";
    synopsis = "Inspect certificates and perform cryptographic operations";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    packageFor = target: targetPkgs: opensslMinimal (staticSetFor target targetPkgs);
  };

  capsh = mkNixpkgsBinary {
    name = "capsh";
    synopsis = "Inspect and modify Linux capability state";
    systems = linuxSystems;
    packageFor = target: targetPkgs: capshMinimal (staticSetFor target targetPkgs);
  };

  getcap = mkNixpkgsBinary {
    name = "getcap";
    synopsis = "Print Linux file capabilities";
    systems = linuxSystems;
    packageFor = target: targetPkgs: capshMinimal (staticSetFor target targetPkgs);
  };

  getpcaps = mkNixpkgsBinary {
    name = "getpcaps";
    synopsis = "Print Linux process capabilities";
    systems = linuxSystems;
    packageFor = target: targetPkgs: capshMinimal (staticSetFor target targetPkgs);
  };

  setcap = mkNixpkgsBinary {
    name = "setcap";
    synopsis = "Set Linux file capabilities";
    systems = linuxSystems;
    packageFor = target: targetPkgs: capshMinimal (staticSetFor target targetPkgs);
  };

  binwalk = mkNixpkgsBinary {
    name = "binwalk";
    synopsis = "Analyze firmware images and embedded files";
    systems = portableSystems;
    packageFor = binwalkPackageFor;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libiconv.2.dylib"];
  };

  ffuf = mkNixpkgsBinary {
    name = "ffuf";
    synopsis = "Fuzz web application paths and parameters";
    systems = portableSystems;
    packageFor = target: targetPkgs:
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "ffuf";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).ffuf;
  };

  gobuster = mkNixpkgsBinary {
    name = "gobuster";
    synopsis = "Enumerate web paths, DNS names, and virtual hosts";
    systems = portableSystems;
    packageFor = gobusterPackageFor;
  };

  gori = mkNixpkgsBinary {
    name = "gori";
    synopsis = "Intercept HTTP traffic and test web applications from the terminal";
    systems = linuxSystems;
    packageFor = target: targetPkgs:
      buildPkgs.callPackage ./gori/default.nix {
        packageSet = packageSetFor target targetPkgs;
      };
  };

  interactsh-client = mkNixpkgsBinary {
    name = "interactsh-client";
    synopsis = "Generate out-of-band testing payloads and collect interactions";
    systems = portableSystems;
    packageFor = interactshClientPackageFor;
  };

  nuclei = mkNixpkgsPackage {
    name = "nuclei";
    synopsis = "Scan targets for known vulnerabilities using bundled templates";
    systems = portableSystems;
    packageFor = nucleiPackageFor;
    entrypoint = "bin/nuclei";
  };
}
