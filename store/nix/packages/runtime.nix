{
  mkNixpkgsBinary,
  mkNixpkgsPackage,
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

  pythonVersion = "3.14";
  version = "3.14.7-web.2";

  pythonSources = {
    aarch64-linux = {
      url = "https://github.com/astral-sh/python-build-standalone/releases/download/20260901/cpython-3.14.7%2B20260901-aarch64-unknown-linux-musl-lto%2Bstatic-full.tar.zst";
      hash = "sha256-OFTmjk6n+/mz9hBwpq0Gbs2D3GctYn2L0eJt73iF0zI=";
    };
    aarch64-darwin = {
      url = "https://github.com/astral-sh/python-build-standalone/releases/download/20260901/cpython-3.14.7%2B20260901-aarch64-apple-darwin-pgo%2Blto-full.tar.zst";
      hash = "sha256-GF+mduFLZIvXNs5/IPmxHiARMbMhbsWjnU7NirinERI=";
    };
    x86_64-linux = {
      url = "https://github.com/astral-sh/python-build-standalone/releases/download/20260901/cpython-3.14.7%2B20260901-x86_64-unknown-linux-musl-lto%2Bstatic-full.tar.zst";
      hash = "sha256-5aduWJPDnIntJorXHD1nlMO+a3I27GE0SRQMV2hvwX8=";
    };
  };

  wheelSources = [
    {
      name = "anyio-4.15.1-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/12/b8/4bd346e22b28902df4d651910f5242c28d84e4a5c2435ca5c3f797ed7e2e/anyio-4.15.1-py3-none-any.whl";
      hash = "sha256-YVL9u/mnf97JdzFyG+v3xMRPfCm0JLAGWCYXPvx+0QE=";
    }
    {
      name = "beautifulsoup4-4.15.0-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/88/c6/92fcd42f1ba33e1184263f25bfabf3d27c383410470f169e4b8163bf9c17/beautifulsoup4-4.15.0-py3-none-any.whl";
      hash = "sha256-1viN5i4dTjjssQd+uXJM0O/ynSoIyhakAem56T8RfPk=";
    }
    {
      name = "certifi-2026.7.22-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/0b/a7/71ac2cff56fec219ed242bb11b8efb69fcc4bec75db06fb7bfe35de520e6/certifi-2026.7.22-py3-none-any.whl";
      hash = "sha256-YvInQrWKGjMBSitrcGWIqNfiqIrnvRpuvoyZKShIN3U=";
    }
    {
      name = "charset_normalizer-3.5.1-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/cc/61/d01fc49b8dea277640b55a9e15960dbca9fdc8c9fde18e572d39c59f4019/charset_normalizer-3.5.1-py3-none-any.whl";
      hash = "sha256-bfDsQw+agxdywjyloiTLo2UXpYqEuzLDK7Wan6Z8R/Y=";
    }
    {
      name = "distro-1.9.0-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/12/b3/231ffd4ab1fc9d679809f356cebee130ac7daa00d6d6f3206dd4fd137e9e/distro-1.9.0-py3-none-any.whl";
      hash = "sha256-e//ZJdZRaPhQJ9jamva92rZYE1uEBnCiI1ibwMjvArI=";
    }
    {
      name = "h11-0.16.0-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/04/4b/29cac41a4d98d144bf5f6d33995617b185d14b22401f75ca86f384e87ff1/h11-0.16.0-py3-none-any.whl";
      hash = "sha256-Y8+LvnUi3jv2WTL9odnCdyBk/7Pa5i1Vky2lSzHLbIY=";
    }
    {
      name = "httpcore-1.0.9-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/7e/f5/f66802a942d491edb555dd61e3a9961140fd64c90bce1eafd741609d334d/httpcore-1.0.9-py3-none-any.whl";
      hash = "sha256-LUAHRqQGaPyd7JgQI5BytAtEhLZAqMOP1lSgJMehv1U=";
    }
    {
      name = "httpx-0.28.1-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/2a/39/e50c7c3a983047577ee07d2a9e53faf5a69493943ec3f6a384bdc792deb2/httpx-0.28.1-py3-none-any.whl";
      hash = "sha256-2Qn8zMEQ+Mf6+BTKgqmk2Ba8Wm2/6iXWWR1phbi6Wa0=";
    }
    {
      name = "idna-3.20-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/58/a2/bb081bab032533a855d44de1d56f8e8426114ff1ba5d1f07a438a0a654f8/idna-3.20-py3-none-any.whl";
      hash = "sha256-q3rnEil0VTNw8L25GeGpYLLNG8HvAnZBbYltuBwUWCw=";
    }
    {
      name = "requests-2.34.2-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/a0/f4/c67b0b3f1b9245e8d266f0f112c500d50e5b4e83cb6f3b71b6528104182a/requests-2.34.2-py3-none-any.whl";
      hash = "sha256-Kg1gwXL4OsarMeRVSQbA87NYjTe1y5ObHAYfSQfieOA=";
    }
    {
      name = "soupsieve-2.9.2-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/eb/dc/ad025c1ee131eba60c69f4dd5779b18fcf1e6b21a343e2162a84d5d133c7/soupsieve-2.9.2-py3-none-any.whl";
      hash = "sha256-gImib9l0ynofMCdtPYSSqyZqsVr1gWQt/oqhYuDByCM=";
    }
    {
      name = "typing_extensions-4.16.0-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/49/d3/b8441a820a491ddfc024b0b0cf0393375b75ea13866d9c66727e54c2fc80/typing_extensions-4.16.0-py3-none-any.whl";
      hash = "sha256-SByqSBN06BPBsXatoU6X8fZ6RTnOnP6z81DXjWNwwug=";
    }
    {
      name = "urllib3-2.8.0-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/92/9d/c4e665119135114480843e7ab388fa94d8480650450e6f8e26b70d323a4c/urllib3-2.8.0-py3-none-any.whl";
      hash = "sha256-DPPK5WjTaqlXayjfs18RMo8cuXTKdkfZR167hsdaxuM=";
    }
    {
      name = "websockets-17.1-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/41/63/23572870e01836a98346075b9e17a8bc24a6ddd9800a3204ceee58677f3c/websockets-17.1-py3-none-any.whl";
      hash = "sha256-8iEIEQe4xIGE2Z9wGWBEhjdufvgmA35wqtawJUBzLCM=";
    }
    {
      name = "pyasn1-0.6.4-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/9a/3b/6163796d69c3977d1e4287bea4a6979161cbbdd170ebb430511e8e1999ce/pyasn1-0.6.4-py3-none-any.whl";
      hash = "sha256-3tqSd8/UVAgOxAsgf7bfgiBqOiaIc1Izzc2NPVZfCIs=";
    }
    {
      name = "PySocks-1.7.1-py3-none-any.whl";
      url = "https://files.pythonhosted.org/packages/8d/59/b4572118e098ac8e46e399a1dd0f2d85403ce8bbaad9ec79373ed6badaf9/PySocks-1.7.1-py3-none-any.whl";
      hash = "sha256-JyW9CpklkZubUXOe6l+eK66R6DKIEIqa0ziy46RDXuU=";
    }
  ];

  wheels = map buildPkgs.fetchurl wheelSources;

  mkPythonWeb = system: let
    source = pythonSources.${system};
  in
    buildPkgs.stdenvNoCC.mkDerivation {
      pname = "python-web";
      inherit version;

      src = buildPkgs.fetchurl {
        name = "python-web-${system}.tar.zst";
        inherit (source) url hash;
      };

      dontUnpack = true;
      nativeBuildInputs = [
        buildPkgs.binutils
        buildPkgs.gnutar
        buildPkgs.unzip
        buildPkgs.zstd
      ];

      installPhase = ''
        runHook preInstall

        archive="$TMPDIR/archive"
        mkdir -p "$archive"
        tar --zstd -xf "$src" -C "$archive" \
          python/install python/licenses python/PYTHON.json
        cp -a "$archive/python/install/." "$out"
        mkdir -p "$out/share/python-build-standalone"
        cp -a "$archive/python/licenses" "$out/share/python-build-standalone/"
        cp "$archive/python/PYTHON.json" "$out/share/python-build-standalone/"
        chmod -R u+w "$out"

        site="$out/lib/python${pythonVersion}/site-packages"
        for wheel in ${toString wheels}; do
          if ! unzip -p "$wheel" '*/WHEEL' | grep -Fqx 'Root-Is-Purelib: true'; then
            echo "python-web: $wheel is not a pure Python wheel" >&2
            exit 1
          fi
          if unzip -Z1 "$wheel" | grep -Eq '\.(so|pyd|dylib)(\.|$)'; then
            echo "python-web: $wheel contains a native extension" >&2
            exit 1
          fi
          unzip -q "$wheel" -d "$site"
        done

        cat > "$site/sitecustomize.py" <<'PY'
        import os

        os.environ.setdefault(
            "SSL_CERT_FILE",
            os.path.join(os.path.dirname(__file__), "certifi", "cacert.pem"),
        )
        PY

        rm -rf \
          "$out/include" \
          "$out/lib/pkgconfig" \
          "$out/lib/libpython${pythonVersion}.a" \
          "$out/lib/libpython${pythonVersion}.dylib" \
          "$out/lib/python${pythonVersion}"/config-* \
          "$out/lib/python${pythonVersion}/ensurepip" \
          "$out/lib/python${pythonVersion}/idlelib" \
          "$out/lib/python${pythonVersion}/site-packages/pip" \
          "$out/lib/python${pythonVersion}"/site-packages/pip-*.dist-info \
          "$out/lib/python${pythonVersion}/test" \
          "$out/lib/python${pythonVersion}/tkinter" \
          "$out/lib/python${pythonVersion}/turtledemo" \
          "$out/lib/python${pythonVersion}/venv" \
          "$out/lib/itcl"* \
          "$out/lib/tcl"* \
          "$out/lib/thread"* \
          "$out/lib/tk"* \
          "$out/share/man"

        find "$out/bin" -mindepth 1 \
          ! -name python \
          ! -name python3 \
          ! -name "python${pythonVersion}" \
          -delete
        find "$site" -type d -name __pycache__ -prune -exec rm -rf {} +
        find "$site" -type f -name '*.pyc' -delete

        ${buildPkgs.lib.optionalString (buildPkgs.lib.hasSuffix "-linux" system) ''
          if readelf -l "$out/bin/python${pythonVersion}" | grep -Fq INTERP; then
            echo "python-web: interpreter has PT_INTERP" >&2
            exit 1
          fi
          if readelf -d "$out/bin/python${pythonVersion}" 2>&1 | grep -Fq NEEDED; then
            echo "python-web: interpreter has DT_NEEDED" >&2
            exit 1
          fi
        ''}
        if find "$site" -type f \( -name '*.so' -o -name '*.pyd' -o -name '*.dylib' \) -print -quit | grep -q .; then
          echo "python-web: site-packages contains a native extension" >&2
          exit 1
        fi
        if grep -R -a -Fq /nix/store/ "$out"; then
          echo "python-web: packaged tree contains a /nix/store reference" >&2
          exit 1
        fi

        runHook postInstall
      '';
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

  python = mkNixpkgsPackage {
    name = "python";
    aliases = ["python3"];
    synopsis = "Static Python with HTTP, WebSocket, and HTML libraries";
    systems = builtins.attrNames pythonSources;
    packageFor = system: _: mkPythonWeb system;
    entrypoint = "bin/python3.14";
  };
}
