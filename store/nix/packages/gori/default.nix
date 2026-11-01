{
  lib,
  stdenvNoCC,
  fetchurl,
  fetchFromGitHub,
  fetchgit,
  linkFarm,
  pkg-config,
  packageSet,
}: let
  # Gori requires Crystal >= 1.22; the pinned nixpkgs compiler is 1.19.
  crystal = stdenvNoCC.mkDerivation {
    pname = "gori-crystal";
    version = "1.22.2";
    src = fetchurl {
      url = "https://github.com/crystal-lang/crystal/releases/download/1.22.2/crystal-1.22.2-1-linux-x86_64-bundled.tar.gz";
      hash = "sha256-DcXD/xtXEMOYHiB6YkH8Bswo+ttmItX9shmZrfHKIwk=";
    };
    dontStrip = true;
    installPhase = ''
      runHook preInstall
      install -Dm755 bin/crystal "$out/bin/crystal"
      mkdir -p "$out/share/crystal"
      cp -R share/crystal/src "$out/share/crystal/"
      runHook postInstall
    '';
  };
  shards = linkFarm "gori-shards" (
    lib.mapAttrsToList (name: source: {
      inherit name;
      path = fetchgit source;
    }) (import ../../assets/gori-shards.nix)
  );
  opensslPortable = packageSet.openssl.overrideAttrs (old: {
    configureFlags =
      (old.configureFlags or [])
      ++ [
        "--prefix=/usr"
        "--openssldir=/etc/ssl"
        "--libdir=lib"
      ];
    outputs = ["out"];
    installPhase = ''
      runHook preInstall
      staged="$(mktemp -d)"
      make install_sw DESTDIR="$staged"
      mkdir -p "$out"
      cp -R "$staged/usr/lib" "$staged/usr/include" "$out/"
      substituteInPlace "$out"/lib/pkgconfig/*.pc \
        --replace-fail 'prefix=/usr' "prefix=$out"
      rm -rf "$staged"
      runHook postInstall
    '';
    postInstall = "";
    postFixup = "";
  });
  libraries = with packageSet; [
    boehmgc
    brotli
    gmp
    libevent
    libyaml
    opensslPortable
    pcre2
    sqlite
    zlib
    zstd
  ];
in
  packageSet.stdenv.mkDerivation {
    pname = "gori";
    version = "0.9.1";
    src = fetchFromGitHub {
      owner = "hahwul";
      repo = "gori";
      rev = "0dcf7e017b8ee49ff6217592ceef18a92475e9bb";
      hash = "sha256-/sgOTe6WEruBfeBcpwy2JMyzYsdx8SXhJfAEsuYSkXY=";
    };
    patches = [
      ../../patches/gori-disable-automatic-update-check.patch
      ../../patches/gori-portable-nix-detection.patch
    ];
    strictDeps = true;
    nativeBuildInputs = [crystal pkg-config];
    buildInputs = libraries;
    env = {
      CRYSTAL_PATH = "lib:crystal-stdlib";
      CRYSTAL_LIBRARY_PATH = lib.makeLibraryPath libraries;
      CC = "${packageSet.stdenv.cc}/bin/${packageSet.stdenv.cc.targetPrefix}cc";
    };
    configurePhase = ''
      runHook preConfigure
      mkdir lib
      cp -RL ${shards}/. lib/
      chmod -R u+w lib
      cp -R ${crystal}/share/crystal/src crystal-stdlib
      export CRYSTAL_CACHE_DIR="$TMPDIR/crystal-cache"
      runHook postConfigure
    '';
    buildPhase = ''
      runHook preBuild
      crystal build src/main.cr -o gori --release --no-debug --static \
        --target ${packageSet.stdenv.hostPlatform.config} --threads "$NIX_BUILD_CORES"
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      install -Dm755 gori "$out/bin/gori"
      runHook postInstall
    '';
    meta = {
      description = "Terminal HTTP intercepting proxy and pentesting toolkit";
      homepage = "https://github.com/hahwul/gori";
      license = lib.licenses.asl20;
      mainProgram = "gori";
      platforms = lib.platforms.linux;
    };
  }
