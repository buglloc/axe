{
  mkNixpkgsBinary,
  buildPkgs,
  axeOpenSslCaBundle,
  linuxSystems,
  portableSystems,
  staticSetFor,
  ...
}: let
  tmuxTerminals = [
    "alacritty"
    "ansi"
    "contour"
    "foot"
    "foot-direct"
    "kitty"
    "linux"
    "rxvt"
    "rxvt-256color"
    "screen"
    "screen-256color"
    "screen.xterm-256color"
    "st"
    "st-256color"
    "tmux"
    "tmux-256color"
    "tmux-direct"
    "vt100"
    "vt220"
    "vte"
    "vte-256color"
    "wezterm"
    "xterm"
    "xterm-256color"
    "xterm-ghostty"
    "xterm-kitty"
    "xterm-direct"
  ];

  tmuxGhosttyTerminfo =
    buildPkgs.runCommand "tmux-ghostty-terminfo-${buildPkgs.ghostty.version}" {
      nativeBuildInputs = [buildPkgs.zig_0_15];
      passAsFile = ["generator"];
      generator = ''
        const std = @import("std");
        const ghostty = @import("ghostty.zig").ghostty;

        pub fn main() !void {
            var buffer: [1024]u8 = undefined;
            var writer = std.fs.File.stdout().writer(&buffer);
            try ghostty.encode(&writer.interface);
            try writer.end();
        }
      '';
    } ''
      cp ${buildPkgs.ghostty.src}/src/terminfo/{ghostty.zig,Source.zig} .
      cp "$generatorPath" main.zig
      zig run main.zig -O ReleaseSafe \
        --cache-dir "$TMPDIR/zig-cache" \
        --global-cache-dir "$TMPDIR/zig-global-cache" > "$out"
    '';

  tmuxNcurses = packageSet:
    (packageSet.ncurses.override {
      enableStatic = true;
      withCxx = false;
    }).overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [buildPkgs.ncurses];
      postPatch =
        (old.postPatch or "")
        + ''
          substituteInPlace ncurses/tinfo/MKfallback.sh \
            --replace-fail '"$tic_path" -o $tmp_info -x "$terminfo_src" >&2' \
              '"$tic_path" -o $tmp_info -x "$terminfo_src" >&2
          "$tic_path" -o $tmp_info -x "${buildPkgs.kitty.src}/terminfo/kitty.terminfo" >&2 || exit 1
          "$tic_path" -o $tmp_info -x "${tmuxGhosttyTerminfo}" >&2 || exit 1'
        '';
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--with-fallbacks=${buildPkgs.lib.concatStringsSep "," tmuxTerminals}"
          "--with-tic-path=${buildPkgs.ncurses}/bin/tic"
          "--with-infocmp-path=${buildPkgs.ncurses}/bin/infocmp"
          "--with-default-terminfo-dir=/usr/share/terminfo"
          "--disable-db-install"
          "--without-tests"
        ];
    });

  tmuxMinimal = packageSet:
    (packageSet.tmux.override {
      ncurses = tmuxNcurses packageSet;
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
