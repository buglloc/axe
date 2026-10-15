{
  mkNixpkgsBinary,
  buildPkgs,
  linuxSystems,
  staticSetFor,
  ...
}: let
  magicDatabase = buildPkgs.file.overrideAttrs (old: {
    # Keep Nix shebang detection without a literal Store path in embedded data.
    patches = (old.patches or []) ++ [../../patches/yara-magic-split-nix-shebang.patch];
  });

  portableOpenSsl = packageSet:
    (packageSet.openssl.override {static = true;}).overrideAttrs (old: {
      postPatch =
        (old.postPatch or "")
        + ''
          substituteInPlace Configurations/unix-Makefile.tmpl \
            --replace-fail 'OPENSSLDIR="\"$(OPENSSLDIR)\""' 'OPENSSLDIR="\"/etc/ssl\""' \
            --replace-fail 'ENGINESDIR="\"$(ENGINESDIR)\""' 'ENGINESDIR="\"/usr/lib/engines\""' \
            --replace-fail 'MODULESDIR="\"$(MODULESDIR)\""' 'MODULESDIR="\"/usr/lib/ossl-modules\""'
        '';
    });

  portableMagic = packageSet:
    packageSet.file.overrideAttrs (old: {
      postConfigure =
        (old.postConfigure or "")
        + ''
          substituteInPlace src/Makefile \
            --replace-fail 'MAGIC = $(pkgdatadir)/magic' 'MAGIC = /usr/share/misc/magic'
        '';
    });

  yaraPackageFor = target: targetPkgs: let
    packageSet = staticSetFor target targetPkgs;
    openssl = portableOpenSsl packageSet;
    file = portableMagic packageSet;
  in
    (packageSet.yara.override {
      inherit openssl file;
      withCrypto = true;
      enableCuckoo = true;
      enableDex = true;
      enableDotNet = true;
      enableMacho = true;
      enableMagic = true;
      enableStatic = true;
    }).overrideAttrs (old: {
      outputs = ["out"];
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [buildPkgs.xxd];
      buildInputs = [openssl file packageSet.jansson packageSet.zlib];
      patches = (old.patches or []) ++ [../../patches/yara-embedded-magic.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          xxd -i < ${magicDatabase}/share/misc/magic.mgc \
            > libyara/modules/magic/axe_magic_database.inc
        '';
      preConfigure =
        (old.preConfigure or "")
        + "\n"
        + ''
          portableLibs="$("$PKG_CONFIG" --static --libs libmagic libcrypto jansson)"
          export LIBS="$portableLibs ''${LIBS:-}"
        '';
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--disable-shared"
          "--disable-pb-tests"
        ];
      doCheck = packageSet.stdenv.buildPlatform.canExecute packageSet.stdenv.hostPlatform;
      installPhase = ''
        runHook preInstall
        install -Dm755 yara "$out/bin/yara"
        install -Dm755 yarac "$out/bin/yarac"
        runHook postInstall
      '';
      postInstall = "";
      preFixup = "";
    });
in {
  yara = mkNixpkgsBinary {
    name = "yara";
    synopsis = "Match user-supplied YARA rules against files and process memory";
    systems = linuxSystems;
    packageFor = yaraPackageFor;
  };

  yarac = mkNixpkgsBinary {
    name = "yarac";
    synopsis = "Compile user-supplied YARA rules for the YARA scanner";
    systems = linuxSystems;
    packageFor = yaraPackageFor;
  };
}
