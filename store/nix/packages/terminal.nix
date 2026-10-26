{
  mkNixpkgsBinary,
  buildPkgs,
  axeOpenSslCaBundle,
  linuxSystems,
  portableSystems,
  staticSetFor,
  packageSetFor,
  portableNcurses,
  ...
}: let
  tmuxMinimal = packageSet:
    (packageSet.tmux.override {
      ncurses = portableNcurses packageSet;
      withSystemd = false;
      withUtempter = false;
    }).overrideAttrs (old: {
      outputs = ["out"];
      configureFlags = (old.configureFlags or []) ++ ["--enable-static" "--with-TERM=tmux-256color"];
      installPhase = ''
        runHook preInstall
        install -Dm755 tmux "$out/bin/tmux"
        runHook postInstall
      '';
      postInstall = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  zellijOpenSsl = packageSet:
    (packageSet.openssl.override {static = true;}).overrideAttrs (old: {
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

  zellijCurl = packageSet: openssl:
    (packageSet.curlMinimal.override {
      inherit openssl;
      opensslSupport = true;
      scpSupport = false;
      gssSupport = false;
    }).overrideAttrs (old: {
      outputs = ["out"];
      separateDebugInfo = false;
      setOutputFlags = false;
      postPatch =
        (old.postPatch or "")
        + ''
          substituteInPlace lib/vtls/openssl.c \
            --replace-fail '#include "vtls/openssl.h"' \
              '#include "vtls/openssl.h"
          #include "axe_zellij_ca_bundle.h"' \
            --replace-fail '#ifdef CURL_CA_FALLBACK' \
              '  if(octx->store_is_empty && !have_native_check) {
              static const struct curl_blob embedded_ca = {
                CURL_UNCONST(axe_zellij_ca_bundle),
                sizeof(axe_zellij_ca_bundle) - 1,
                CURL_BLOB_NOCOPY
              };
              result = load_cacert_from_memory(store, &embedded_ca);
              if(result) {
                failf(data, "error adding trust anchors from embedded certificate bundle: %d",
                      (int)result);
                return result;
              }
              infof(data, "  CA Blob from embedded bundle");
              octx->store_is_empty = FALSE;
            }

          #ifdef CURL_CA_FALLBACK'
        '';
      preBuild =
        (old.preBuild or "")
        + ''
          perl src/mk-file-embed.pl --var axe_zellij_ca_bundle \
            < ${axeOpenSslCaBundle} > lib/vtls/axe_zellij_ca_bundle.h
          substituteInPlace lib/vtls/axe_zellij_ca_bundle.h \
            --replace-fail '#include "tool_setup.h"' '#include "curl_setup.h"' \
            --replace-fail 'const unsigned char axe_zellij_ca_bundle[]' \
              'static const unsigned char axe_zellij_ca_bundle[]'
        '';
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--disable-docs"
          "--without-ca-bundle"
          "--without-ca-fallback"
          "--without-ca-path"
        ];
      buildPhase = ''
        runHook preBuild
        make -C lib -j"$NIX_BUILD_CORES"
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        staged="$(mktemp -d)"
        make -C lib install DESTDIR="$staged"
        make -C include install DESTDIR="$staged"
        mkdir -p "$out"
        cp -R "$staged/usr/lib" "$staged/usr/include" "$out/"
        install -Dm644 libcurl.pc "$out/lib/pkgconfig/libcurl.pc"
        substituteInPlace "$out/lib/pkgconfig/libcurl.pc" \
          --replace-fail 'prefix=/usr' "prefix=$out"
        rm -rf "$staged"
        runHook postInstall
      '';
      postInstall = "";
      outputChecks = {
        out = old.outputChecks.out or {};
      };
    });

  zellijMinimal = packageSet: let
    openssl = zellijOpenSsl packageSet;
    curl = zellijCurl packageSet openssl;
  in
    (packageSet.zellij-unwrapped.override {
      inherit curl openssl;
    }).overrideAttrs (old: {
      buildNoDefaultFeatures = true;
      buildFeatures = ["web_server_capability"];
      env =
        (old.env or {})
        // {
          OPENSSL_STATIC = "1";
          PKG_CONFIG_ALL_STATIC = "1";
          PREFIX = "/usr";
          RUSTFLAGS = (old.env.RUSTFLAGS or "") + " -C relocation-model=static";
        };
      # curl-sys invokes curl-config on the build host, even for cross builds.
      nativeBuildInputs =
        (buildPkgs.lib.filter (input: input != buildPkgs.lib.getDev curl) (old.nativeBuildInputs or []))
        ++ [(buildPkgs.lib.getDev buildPkgs.curlMinimal)];
      outputs = ["out"];
      postInstall = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  zellijDarwin = buildPkgs.stdenvNoCC.mkDerivation {
    pname = "zellij";
    version = buildPkgs.zellij-unwrapped.version;
    archive = buildPkgs.fetchurl {
      url = "https://github.com/zellij-org/zellij/releases/download/v${buildPkgs.zellij-unwrapped.version}/zellij-aarch64-apple-darwin.tar.gz";
      hash = "sha256-wCm6T+GSe3mtnwzdWRVcTf+Ad3hjyFhX1NCbiLVvmJE=";
    };
    dontUnpack = true;
    nativeBuildInputs = [buildPkgs.gnutar];
    installPhase = ''
      runHook preInstall
      tar -xzf "$archive" zellij
      install -Dm755 zellij "$out/bin/zellij"
      runHook postInstall
    '';
    meta.mainProgram = "zellij";
  };
in {
  asciinema = mkNixpkgsBinary {
    name = "asciinema";
    synopsis = "Record, replay, and stream terminal sessions";
    systems = portableSystems;
    darwinSystemLibraries = ["libiconv.2.dylib"];
    packageFor = system: pkgs:
      if system == "aarch64-darwin"
      then pkgs.asciinema
      else
        (packageSetFor system pkgs).asciinema.overrideAttrs (old: {
          patches = (old.patches or []) ++ [../patches/asciinema-pty-spawn.patch];
          postInstall = "";
          doInstallCheck = system == "x86_64-linux";
          postInstallCheck = ''
            recording="$(mktemp -d)"
            status=0
            ASCIINEMA_CONFIG_HOME="$recording/config" \
              ASCIINEMA_STATE_HOME="$recording/state" \
              timeout 15 "$out/bin/asciinema" rec --headless --return --quiet \
                --command 'printf "session:%s\n" "$ASCIINEMA_SESSION"; exit 7' \
                "$recording/session.cast" || status=$?
            test "$status" -eq 7
            "$out/bin/asciinema" convert "$recording/session.cast" "$recording/session.txt"
            grep -Eq '^session:[0-9a-f]+' "$recording/session.txt"
            rm -rf "$recording"
          '';
        });
  };

  tmux = mkNixpkgsBinary {
    name = "tmux";
    synopsis = "Multiplex terminal sessions and windows";
    systems = linuxSystems;
    packageFor = system: pkgs: tmuxMinimal (staticSetFor system pkgs);
  };

  zellij = mkNixpkgsBinary {
    name = "zellij";
    synopsis = "Manage terminal sessions, panes, and embedded plugins";
    systems = portableSystems;
    packageFor = system: pkgs:
      if system == "aarch64-darwin"
      then zellijDarwin
      else zellijMinimal (staticSetFor system pkgs);
  };
}
