{
  mkNixpkgsBinary,
  mkNixpkgsPackage,
  mkUpstreamBinaries,
  pkgsFor,
  buildPkgs,
  axeOpenSslCaBundle,
  axePortableCaBundle,
  linuxSystems,
  portableSystems,
  staticSetFor,
  packageSetFor,
  targetStrip,
  goDarwin,
  disableCdncheckIPv6Probe,
  ...
}: let
  libpcapMinimal = staticSet: staticSet.libpcap.override {withBluez = false;};
  tsharkMinimal = staticSet:
    (staticSet.wireshark-cli.override {
      asciidoctor = null;
      bcg729 = null;
      gettext = null;
      gnutls = null;
      libcap = null;
      libkrb5 = null;
      libmaxminddb = null;
      libnl = null;
      libopus = null;
      libsmi = null;
      libssh = null;
      lua5_4 = null;
      lz4 = null;
      makeWrapper = null;
      minizip = null;
      nghttp2 = null;
      nghttp3 = null;
      opencore-amr = null;
      openssl = null;
      sbc = null;
      snappy = null;
      spandsp3 = null;
      speexdsp = null;
      zstd = null;
      brotli = null;
      withExtras = false;
      libpcap' = libpcapMinimal staticSet;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      buildInputs = [
        staticSet.c-ares
        staticSet.glib
        staticSet.libgcrypt
        staticSet.libgpg-error
        (libpcapMinimal staticSet)
        staticSet.libxml2
        staticSet.pcre2
        staticSet.zlib-ng
      ];
      cmakeFlags =
        (old.cmakeFlags or [])
        ++ [
          "-DBUILD_SHARED_LIBS=OFF"
          "-DUSE_STATIC=ON"
          "-DENABLE_PLUGINS=OFF"
          "-DBUILD_wireshark=OFF"
          "-DBUILD_stratoshark=OFF"
          "-DBUILD_tshark=ON"
          "-DBUILD_strato=OFF"
          "-DBUILD_tfshark=OFF"
          "-DBUILD_rawshark=OFF"
          "-DBUILD_dumpcap=ON"
          "-DBUILD_text2pcap=OFF"
          "-DBUILD_mergecap=OFF"
          "-DBUILD_reordercap=OFF"
          "-DBUILD_editcap=OFF"
          "-DBUILD_capinfos=OFF"
          "-DBUILD_captype=OFF"
          "-DBUILD_randpkt=OFF"
          "-DBUILD_dftest=OFF"
          "-DBUILD_dcerpcidl2wrs=OFF"
          "-DBUILD_androiddump=OFF"
          "-DBUILD_sshdump=OFF"
          "-DBUILD_ciscodump=OFF"
          "-DBUILD_dpauxmon=OFF"
          "-DBUILD_randpktdump=OFF"
          "-DBUILD_wifidump=OFF"
          "-DBUILD_sdjournal=OFF"
          "-DBUILD_udpdump=OFF"
          "-DBUILD_sharkd=OFF"
          "-DBUILD_mmdbresolve=OFF"
          "-DENABLE_ZLIB=OFF"
          "-DENABLE_ZLIBNG=ON"
          "-DENABLE_XXHASH=OFF"
          "-DENABLE_MINIZIP=OFF"
          "-DENABLE_MINIZIPNG=OFF"
          "-DENABLE_LZ4=OFF"
          "-DENABLE_BROTLI=OFF"
          "-DENABLE_SNAPPY=OFF"
          "-DENABLE_ZSTD=OFF"
          "-DENABLE_NGHTTP2=OFF"
          "-DENABLE_NGHTTP3=OFF"
          "-DENABLE_LUA=OFF"
          "-DENABLE_SMI=OFF"
          "-DENABLE_GNUTLS=OFF"
          "-DENABLE_CAP=OFF"
          "-DENABLE_NETLINK=OFF"
          "-DENABLE_KERBEROS=OFF"
          "-DENABLE_SBC=OFF"
          "-DENABLE_SPANDSP=OFF"
          "-DENABLE_BCG729=OFF"
          "-DENABLE_AMRNB=OFF"
          "-DENABLE_ILBC=OFF"
          "-DENABLE_OPUS=OFF"
          "-DENABLE_SINSP=OFF"
        ];
      postConfigure =
        (old.postConfigure or "")
        + ''
          sed -i 's/;/" "/g' \
            "$NIX_BUILD_TOP/source/build/CMakeFiles/tshark.dir/link.txt" \
            "$NIX_BUILD_TOP/source/build/CMakeFiles/dumpcap.dir/link.txt"
        '';
      buildPhase = ''
        runHook preBuild
        cmake --build "$NIX_BUILD_TOP/source/build" --target tshark
        cmake --build "$NIX_BUILD_TOP/source/build" --target dumpcap
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 "$NIX_BUILD_TOP/source/build/run/tshark" "$out/bin/tshark"
        install -Dm755 "$NIX_BUILD_TOP/source/build/run/dumpcap" "$out/bin/dumpcap"
        ${targetStrip staticSet} -s "$out/bin/tshark" "$out/bin/dumpcap"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
    });

  ssMinimal = staticSet:
    buildPkgs.runCommand "ss-${buildPkgs.iproute2.version}"
    {
      inherit (buildPkgs.iproute2) version;
    }
    ''
      install -Dm755 ${staticSet.iproute2}/sbin/ss "$out/bin/ss"
    '';

  darwinOpenSshLibraries = [
    "libedit.3.dylib"
    "libresolv.9.dylib"
    "libutil.dylib"
    "libz.dylib"
  ];

  opensshClientMinimal = packageSet:
    (packageSet.openssh.override {
      withLdns = false;
      withLinuxMemlock = false;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--sysconfdir=/etc/ssh"
        ];
      buildPhase = ''
        runHook preBuild
        make SSH_PROGRAM=ssh ssh scp sftp ssh-add ssh-agent ssh-keygen ssh-keyscan
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 ssh scp sftp ssh-add ssh-agent ssh-keygen ssh-keyscan -t "$out/bin"
        ${targetStrip packageSet} -s "$out/bin/ssh" "$out/bin/scp" "$out/bin/sftp" "$out/bin/ssh-add" "$out/bin/ssh-agent" "$out/bin/ssh-keygen" "$out/bin/ssh-keyscan"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  opensshPackageFor = target: targetPkgs:
    if target == "aarch64-darwin"
    then opensshClientMinimal (staticSetFor target targetPkgs)
    else if target == "aarch64-linux"
    then opensshClientMinimal buildPkgs.pkgsCross.aarch64-multiplatform-musl.pkgsStatic
    else opensshClientMinimal targetPkgs.pkgsStatic;

  naabuPatches = [
    ../patches/naabu-disable-updates.patch
    ../patches/naabu-fully-static.patch
  ];
  naabuVendorHash = "sha256-+7oSwkFvnkmP8VLsQqHASktY8AS7bgyGO1IJoIjnNgY=";
  naabuMinimal = staticSet:
    disableCdncheckIPv6Probe (
      staticSet.naabu.overrideAttrs (old: {
        patches = (old.patches or []) ++ naabuPatches;
        vendorHash = naabuVendorHash;
        ldflags = (old.ldflags or []) ++ ["-w"];
      })
    );

  # The fully-static patch removes the runtime libpcap dependency. Build the
  # Darwin binary with CGO disabled so it only references macOS system APIs.
  naabuDarwin = disableCdncheckIPv6Probe (
    (goDarwin {
      package = "naabu";
      subPackage = "./cmd/naabu";
    }).overrideAttrs
    (old: {
      patches = (old.patches or []) ++ naabuPatches;
      vendorHash = naabuVendorHash;
    })
  );

  grpcurlPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "grpcurl";
          subPackage = "./cmd/grpcurl";
        }
      else (packageSetFor target targetPkgs).grpcurl
    ).overrideAttrs
    (old: {
      patches = (old.patches or []) ++ [../patches/grpcurl-bundled-roots.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} axe-ca-bundle.pem
        '';
    });

  curlMinimal = staticSet:
    (staticSet.curlMinimal.override {
      brotliSupport = false;
      c-aresSupport = false;
      gssSupport = false;
      http3Support = false;
      idnSupport = false;
      ldapSupport = false;
      opensslSupport = true;
      pslSupport = false;
      scpSupport = false;
      rustlsSupport = false;
      websocketSupport = false;
      zstdSupport = false;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      separateDebugInfo = false;
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--disable-alt-svc"
          "--disable-dict"
          "--disable-docs"
          "--disable-ftp"
          "--disable-gopher"
          "--disable-hsts"
          "--disable-imap"
          "--disable-ipfs"
          "--disable-libcurl-option"
          "--disable-mqtt"
          "--disable-pop3"
          "--disable-rtsp"
          "--disable-smb"
          "--disable-smtp"
          "--disable-telnet"
          "--disable-tftp"
          "--without-ca-bundle"
          "--without-ca-fallback"
          "--without-ca-path"
          "--with-ca-embed=${axeOpenSslCaBundle}"
        ];
      postInstall = "";
      postFixup =
        (old.postFixup or "")
        + ''
          sed -i 's|/nix/store/|/non/store/|g' "$out/bin/curl"
        '';
      installPhase = ''
        runHook preInstall
        make -C src install-binPROGRAMS bindir="$out/bin"
        ${targetStrip staticSet} -s "$out/bin/curl"
        runHook postInstall
      '';
    });

  xhMinimal = packageSet:
    (packageSet.xh.override {
      withNativeTls = false;
    }).overrideAttrs
    (old: {
      patches = (old.patches or []) ++ [../patches/xh-bundled-roots.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          cp ${axePortableCaBundle} src/axe-ca-bundle.pem
        '';
      buildNoDefaultFeatures = true;
      buildFeatures = ["rustls"];
      outputs = ["out"];
      postInstall = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  websocatMinimal = staticSet:
    (staticSet.websocat.override {
      openssl = portableOpenSsl staticSet;
    }).overrideAttrs (old: {
      env =
        (old.env or {})
        // {
          OPENSSL_STATIC = "1";
          PKG_CONFIG_ALL_STATIC = "1";
        };
      postPatch =
        (old.postPatch or "")
        + ''
              cp ${axePortableCaBundle} src/axe-ca-bundle.pem
              substituteInPlace src/ssl_peer.rs \
                --replace-fail 'let mut b = TlsConnector::builder();' \
                  'let mut b = tls_connector_builder()?;'
              substituteInPlace src/ws_client_peer.rs \
                --replace-fail 'let mut builder_ = super::ssl_peer::native_tls::TlsConnector::builder();' \
                  'let mut builder_ = super::ssl_peer::tls_connector_builder()?;'
              cat >> src/ssl_peer.rs <<'EOF'

          pub(super) fn tls_connector_builder() -> native_tls::Result<native_tls::TlsConnectorBuilder> {
              let mut builder = TlsConnector::builder();
              for pem in include_str!("axe-ca-bundle.pem").split_inclusive("-----END CERTIFICATE-----") {
                  if pem.contains("-----BEGIN CERTIFICATE-----") {
                      builder.add_root_certificate(native_tls::Certificate::from_pem(pem.as_bytes())?);
                  }
              }
              Ok(builder)
          }
          EOF
        '';
      outputs = ["out"];
      postInstall = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  nmapMinimal = {
    packageSet,
    dependencySet,
    useBundledLibraries ? false,
  }: let
    libpcap = libpcapMinimal dependencySet;
  in
    (packageSet.nmap.override {
      withLua = true;
      inherit libpcap;
      openssl = dependencySet.openssl;
      lua5_4 = dependencySet.lua5_4;
      pcre2 = dependencySet.pcre2;
      libssh2 = dependencySet.libssh2;
      zlib = dependencySet.zlib;
    }).overrideAttrs
    (old: {
      enableParallelBuilding = false;
      outputs = ["out"];
      configureFlags =
        (
          if useBundledLibraries
          then
            builtins.filter
            (flag: builtins.match "--with-liblua=.*" flag == null)
            (old.configureFlags or [])
          else (old.configureFlags or [])
        )
        ++ [
          "--prefix=/usr"
          "--disable-nls"
          "--without-ndiff"
          "--without-nping"
          "--without-zenmap"
        ]
        ++ (
          if useBundledLibraries
          then ["--with-liblua=included"]
          else []
        );
      buildInputs =
        if useBundledLibraries
        then [
          dependencySet.pcre2
          dependencySet.libssh2
          libpcap
          dependencySet.openssl
          dependencySet.zlib
        ]
        else old.buildInputs;
      postPatch =
        (old.postPatch or "")
        + ''
          substituteInPlace services.cc \
            --replace-fail '  ratio_format = 0;' $'  ratio_format = 0;\n  services_initialized = 1;\n  return 0;'
        '';
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
      postInstall = "";
      postFixup = "";
      installPhase = ''
        runHook preInstall
        install -Dm755 nmap "$out/bin/nmap"
        install -Dm755 ncat/ncat "$out/bin/ncat"
        ${targetStrip packageSet} -x -s "$out/bin/nmap" "$out/bin/ncat"
        mkdir -p "$out/share/nmap/scripts" "$out/share/nmap/nselib"
        install -m 644 nse_main.lua "$out/share/nmap/"
        install -m 644 scripts/script.db scripts/*.nse "$out/share/nmap/scripts/"
        install -m 644 nselib/*.lua "$out/share/nmap/nselib/"
        cp -a nselib/data "$out/share/nmap/nselib/"
        install -m 644 nmap-services nmap-rpc nmap-os-db nmap-service-probes \
          nmap-protocols nmap-mac-prefixes "$out/share/nmap/"

        ${packageSet.buildPackages.perl}/bin/perl -0pi -e 's{/nix/store/}{/usr/local/}g' \
          "$out/bin/nmap" "$out/bin/ncat"
        if grep -R -a -Fq /nix/store/ "$out"; then
          echo "nmap: packaged tree contains a /nix/store reference" >&2
          exit 1
        fi
        runHook postInstall
      '';
    });

  # Darwin uses its native stdenv while third-party libraries stay static.
  # Bundled Lua and liblinear avoid store-backed dylibs without pkgsStatic's
  # cross-style compiler name.
  nmapPackageFor = target: targetPkgs: let
    dependencySet = staticSetFor target targetPkgs;
    packageSet =
      if target == "aarch64-darwin"
      then targetPkgs
      else dependencySet;
  in
    nmapMinimal {
      inherit packageSet dependencySet;
      useBundledLibraries = target == "aarch64-darwin";
    };

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

  masscanMinimal = staticSet:
    (staticSet.masscan.override {
      libpcap = staticSet.libpcap;
    }).overrideAttrs (old: {
      outputs = ["out"];
      nativeBuildInputs = [];
      buildInputs = [staticSet.libpcap];
      makeFlags =
        (old.makeFlags or [])
        ++ [
          "LDFLAGS=-L${staticSet.libpcap.lib}/lib -lpcap"
        ];
      postInstall = "";
      installPhase = ''
        runHook preInstall
        install -Dm755 bin/masscan "$out/bin/masscan"
        ${targetStrip staticSet} -s "$out/bin/masscan"
        runHook postInstall
      '';
      doInstallCheck = false;
      installCheckPhase = "";
    });

  portableOpenSsl = staticSet:
    staticSet.openssl.overrideAttrs (old: {
      postPatch =
        (old.postPatch or "")
        + ''
          substituteInPlace Configurations/unix-Makefile.tmpl \
            --replace-fail 'OPENSSLDIR="\"$(OPENSSLDIR)\""' 'OPENSSLDIR="\"/etc/ssl\""' \
            --replace-fail 'ENGINESDIR="\"$(ENGINESDIR)\""' 'ENGINESDIR="\"/usr/lib/engines\""' \
            --replace-fail 'MODULESDIR="\"$(MODULESDIR)\""' 'MODULESDIR="\"/usr/lib/ossl-modules\""'
        '';
    });

  digMinimal = staticSet: let
    openssl = portableOpenSsl staticSet;
  in
    (staticSet.bind.override {
      enableGSSAPI = false;
      inherit openssl;
    }).overrideAttrs (old: {
      outputs = ["out"];
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [buildPkgs.xxd];
      buildInputs = [
        staticSet.libcap
        staticSet.libidn2
        staticSet.liburcu
        staticSet.libuv
        staticSet.nghttp2
        staticSet.zlib
        openssl
      ];
      patches = (old.patches or []) ++ [../patches/bind-dig-static-portable.patch];
      postPatch =
        (old.postPatch or "")
        + ''
          xxd -i < ${axePortableCaBundle} > lib/isc/axe_ca_bundle.inc
        '';
      configureFlags =
        [
          "--prefix=/usr"
          "--sysconfdir=/etc"
          "--localstatedir=/var"
          "--enable-static"
          "--disable-shared"
          "--disable-dnstap"
          "--disable-geoip"
          "--without-gssapi"
          "--without-lmdb"
          "--without-libxml2"
          "--without-json-c"
          "--without-readline"
          "--without-jemalloc"
          "--without-cmocka"
          "--with-libidn2"
          "--with-libnghttp2=yes"
          "--with-zlib=yes"
        ]
        ++ buildPkgs.lib.optional (
          staticSet.stdenv.hostPlatform != staticSet.stdenv.buildPlatform
        ) "BUILD_CC=$(CC_FOR_BUILD)";
      buildPhase = ''
        runHook preBuild
        make -C lib -j"$NIX_BUILD_CORES"
        make -C bin/dig -j"$NIX_BUILD_CORES" dig
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 bin/dig/dig "$out/bin/dig"
        ${targetStrip staticSet} -s "$out/bin/dig"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doCheck = false;
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  ethtoolMinimal = staticSet:
    staticSet.ethtool.overrideAttrs (old: {
      outputs = ["out"];
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--without-bash-completion-dir"
        ];
      installPhase = ''
        runHook preInstall
        install -Dm755 ethtool "$out/bin/ethtool"
        ${targetStrip staticSet} -s "$out/bin/ethtool"
        runHook postInstall
      '';
      postInstall = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
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
  tcpdump = mkNixpkgsBinary {
    name = "tcpdump";
    synopsis = "Capture and inspect network packets";
    systems = portableSystems;
    packageFor = target: targetPkgs: (staticSetFor target targetPkgs).tcpdump;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libpcap.A.dylib"];
  };

  tshark = mkNixpkgsBinary {
    name = "tshark";
    synopsis = "Analyze network captures from the command line";
    systems = linuxSystems;
    packageFor = target: targetPkgs: tsharkMinimal (staticSetFor target targetPkgs);
    rewriteBuildConfigurationPaths = true;
  };

  dumpcap = mkNixpkgsBinary {
    name = "dumpcap";
    synopsis = "Capture packets for Wireshark command-line tools";
    systems = linuxSystems;
    packageFor = target: targetPkgs: tsharkMinimal (staticSetFor target targetPkgs);
    rewriteBuildConfigurationPaths = true;
  };

  ss = mkNixpkgsBinary {
    name = "ss";
    synopsis = "Inspect sockets and network connections";
    systems = linuxSystems;
    packageFor = target: targetPkgs: ssMinimal (staticSetFor target targetPkgs);
    rewriteBuildConfigurationPaths = true;
  };

  socat = mkNixpkgsBinary {
    name = "socat";
    synopsis = "Relay bidirectional data between sockets and streams";
    systems = portableSystems;
    packageFor = target: targetPkgs: (staticSetFor target targetPkgs).socat;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = [
      "libresolv.9.dylib"
      "libutil.dylib"
    ];
  };

  websocat = mkNixpkgsBinary {
    name = "websocat";
    synopsis = "Relay data between WebSockets and streams";
    systems = linuxSystems;
    packageFor = target: targetPkgs: websocatMinimal (staticSetFor target targetPkgs);
  };

  openssh = mkNixpkgsBinary {
    name = "ssh";
    synopsis = "Connect securely to remote hosts with OpenSSH";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
  };

  scp = mkNixpkgsBinary {
    name = "scp";
    synopsis = "Copy files securely between hosts with OpenSSH";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
  };

  sftp = mkNixpkgsBinary {
    name = "sftp";
    synopsis = "Transfer files through OpenSSH SFTP";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
  };

  ssh-add = mkNixpkgsBinary {
    name = "ssh-add";
    synopsis = "Add OpenSSH private keys to an authentication agent";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
  };

  ssh-agent = mkNixpkgsBinary {
    name = "ssh-agent";
    synopsis = "Hold OpenSSH private keys for authenticated sessions";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
  };

  ssh-keygen = mkNixpkgsBinary {
    name = "ssh-keygen";
    synopsis = "Generate and manage OpenSSH authentication keys";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
  };

  ssh-keyscan = mkNixpkgsBinary {
    name = "ssh-keyscan";
    synopsis = "Collect OpenSSH host public keys";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = darwinOpenSshLibraries;
    packageFor = opensshPackageFor;
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

  curl = mkNixpkgsBinary {
    name = "curl";
    synopsis = "Rock solid HTTP client";
    systems = portableSystems;
    packageFor = target: targetPkgs: curlMinimal (staticSetFor target targetPkgs);
  };

  grpcurl = mkNixpkgsBinary {
    name = "grpcurl";
    synopsis = "Call and inspect gRPC services";
    systems = portableSystems;
    packageFor = grpcurlPackageFor;
  };

  xh = mkNixpkgsBinary {
    name = "xh";
    synopsis = "Fancy HTTP client";
    systems = portableSystems;
    packageFor = target: targetPkgs: xhMinimal (packageSetFor target targetPkgs);
    darwinSystemLibraries = ["libiconv.2.dylib"];
  };

  ncat = mkNixpkgsBinary {
    name = "ncat";
    aliases = ["nc"];
    synopsis = "Connect, listen, redirect, and proxy network traffic";
    systems = portableSystems;
    packageFor = nmapPackageFor;
  };

  naabu = mkNixpkgsBinary {
    name = "naabu";
    synopsis = "Scan hosts for open ports";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    packageFor = target: targetPkgs:
      if target == "aarch64-darwin"
      then naabuDarwin
      else naabuMinimal (packageSetFor target targetPkgs);
  };

  nmap = mkNixpkgsPackage {
    name = "nmap";
    synopsis = "Discover hosts and audit network services with NSE scripting";
    systems = portableSystems;
    packageFor = nmapPackageFor;
    entrypoint = "bin/nmap";
  };

  httpx = mkNixpkgsBinary {
    name = "httpx";
    synopsis = "Probe HTTP services and identify live web targets";
    systems = portableSystems;
    packageFor = target: targetPkgs:
      disableCdncheckIPv6Probe (
        (
          if target == "aarch64-darwin"
          then
            goDarwin {
              package = "httpx";
              subPackage = "./cmd/httpx";
            }
          else (packageSetFor target targetPkgs).httpx
        ).overrideAttrs
        (old: {
          patches = (old.patches or []) ++ [../patches/httpx-axe-defaults.patch];
        })
      );
  };

  subfinder = mkNixpkgsBinary {
    name = "subfinder";
    synopsis = "Enumerate subdomains from passive and active sources";
    systems = portableSystems;
    packageFor = target: targetPkgs:
      disableCdncheckIPv6Probe (
        if target == "aarch64-darwin"
        then
          goDarwin {
            package = "subfinder";
            subPackage = "./cmd/subfinder";
          }
        else (packageSetFor target targetPkgs).subfinder
      );
  };

  dnsx = mkNixpkgsBinary {
    name = "dnsx";
    synopsis = "Resolve and enumerate DNS records";
    systems = portableSystems;
    packageFor = target: targetPkgs:
      disableCdncheckIPv6Probe (
        (
          if target == "aarch64-darwin"
          then
            goDarwin {
              package = "dnsx";
              subPackage = "./cmd/dnsx";
            }
          else (packageSetFor target targetPkgs).dnsx
        ).overrideAttrs
        (old: {
          patches = (old.patches or []) ++ [../patches/dnsx-disable-automatic-update-check.patch];
        })
      );
  };

  yc = mkUpstreamBinaries {
    name = "yc";
    version = "1.25.0";
    synopsis = "Manage Yandex Cloud resources";
    sources = {
      x86_64-linux = {
        url = "https://storage.yandexcloud.net/yandexcloud-yc/release/1.25.0/linux/amd64/yc";
        hash = "sha256-7lnoJM4gILuuAMA/do65obg6bqqMsaIW4sgsmMBqWLg=";
      };
      aarch64-linux = {
        url = "https://storage.yandexcloud.net/yandexcloud-yc/release/1.25.0/linux/arm64/yc";
        hash = "sha256-2z6qThMXOoN15EaheESrq/xHD49/F5Q401U8oRDwW+k=";
        buildSystem = "x86_64-linux";
      };
      aarch64-darwin = {
        url = "https://storage.yandexcloud.net/yandexcloud-yc/release/1.25.0/darwin/arm64/yc";
        hash = "sha256-1hwZh8b9FnYjhryJ/XrFUBm1F4SJVLERwLfCSVJvfZI=";
        buildSystem = "x86_64-linux";
      };
    };
  };

  masscan = mkNixpkgsBinary {
    name = "masscan";
    synopsis = "Scan large networks for open ports at high speed";
    systems = linuxSystems;
    packageFor = target: targetPkgs: masscanMinimal (staticSetFor target targetPkgs);
  };

  dig = mkNixpkgsBinary {
    name = "dig";
    synopsis = "Query DNS records with the BIND DNS client";
    systems = linuxSystems;
    packageFor = target: targetPkgs: digMinimal (staticSetFor target targetPkgs);
  };

  ethtool = mkNixpkgsBinary {
    name = "ethtool";
    synopsis = "Inspect and configure Linux network device parameters";
    systems = linuxSystems;
    packageFor = target: targetPkgs: ethtoolMinimal (staticSetFor target targetPkgs);
  };

  rclone = mkNixpkgsBinary {
    name = "rclone";
    synopsis = "Copy and synchronize files with remote storage";
    systems = portableSystems;
    packageFor = rclonePackageFor;
  };
}
