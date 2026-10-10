{
  mkNixpkgsBinary,
  buildPkgs,
  pkgsFor,
  linuxSystems,
  portableSystems,
  staticSetFor,
  packageSetFor,
  targetStrip,
  ...
}: let
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

  bpftoolVersion = "7.7.0";
  bpftoolSources = {
    aarch64-linux = {
      url = "https://github.com/libbpf/bpftool/releases/download/v${bpftoolVersion}/bpftool-v${bpftoolVersion}-arm64.tar.gz";
      hash = "sha256-1oRD2UXBRggBUeshW+uqQAKUq3raIq9PXQqUD1mSudk=";
    };
    x86_64-linux = {
      url = "https://github.com/libbpf/bpftool/releases/download/v${bpftoolVersion}/bpftool-v${bpftoolVersion}-amd64.tar.gz";
      hash = "sha256-CRUFlvCTVrD/Yy3X+YVuDqhr+WsmnpvJQnjZqUMqJow=";
    };
  };
  bpftoolPackage = system: let
    source = bpftoolSources.${system};
    archive = buildPkgs.fetchurl {
      inherit (source) url hash;
    };
  in
    buildPkgs.runCommand "bpftool-${bpftoolVersion}-${system}"
    {
      version = bpftoolVersion;
      nativeBuildInputs = [
        buildPkgs.gnutar
        buildPkgs.gzip
      ];
    }
    ''
      tar -xzf ${archive} bpftool
      install -Dm755 bpftool "$out/bin/bpftool"
    '';
  elfutilsSetFor = target: let
    packageSet =
      if target == "aarch64-linux"
      then buildPkgs.pkgsCross.aarch64-multiplatform-musl.pkgsMusl
      else buildPkgs.pkgsMusl;
  in
    packageSet.extend (_: previous: {
      sqlite = previous.sqlite.overrideAttrs (_: {
        doCheck = false;
        doInstallCheck = false;
      });
    });
  dbusMonitorMinimal = staticSet:
    (staticSet.dbus.override {
      enableSystemd = false;
      x11Support = false;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      separateDebugInfo = false;
      mesonFlags = (old.mesonFlags or []) ++ ["--prefix=/usr"];
      buildPhase = ''
        runHook preBuild
        ninja tools/dbus-monitor
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 tools/dbus-monitor "$out/bin/dbus-monitor"
        ${targetStrip staticSet} -s "$out/bin/dbus-monitor"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doCheck = false;
    });
  elfutilsMusl = target:
    ((elfutilsSetFor target).elfutils.override {enableDebuginfod = false;}).overrideAttrs (_: {
      doCheck = false;
      doInstallCheck = false;
    });
  ltraceMinimal = staticSet: target:
    (staticSet.ltrace.override {
      elfutils = elfutilsMusl target;
      dejagnu = null;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      postPatch =
        (old.postPatch or "")
        + ''
          substituteInPlace configure.ac \
            --replace-fail 'linux-gnu*) HOST_OS' 'linux-gnu*|linux-musl*) HOST_OS'
          substituteInPlace proc.h \
            --replace-fail '#include <sys/time.h>' $'#include <sys/types.h>\n#include <sys/time.h>'
        '';
      buildInputs =
        (old.buildInputs or [])
        ++ [
          staticSet.zlib
          staticSet.zstd
        ];
      configureFlags = (old.configureFlags or []) ++ ["--prefix=/usr"];
      LIBS = "-lz -lzstd";
      installPhase = ''
        runHook preInstall
        install -Dm755 ltrace "$out/bin/ltrace"
        ${targetStrip staticSet} -s "$out/bin/ltrace"
        runHook postInstall
      '';
      postFixup = "";
      doCheck = false;
      nativeCheckInputs = [];
    });
  gdbMinimal = staticSet:
    (staticSet.gdb.override {
      withTui = false;
      pythonSupport = false;
      enableDebuginfod = false;
      hostCpuOnly = true;
      dejagnu = null;
      sourceHighlight = null;
      safePaths = [];
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--without-guile"
          "--disable-nls"
          "--prefix=/usr"
          "--datadir=/usr/share"
        ];
      buildPhase = ''
        runHook preBuild
        make all-gdb all-gdbserver
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 gdb/gdb gdbserver/gdbserver -t "$out/bin"
        ${targetStrip staticSet} -s "$out/bin/gdb" "$out/bin/gdbserver"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doCheck = false;
      doInstallCheck = false;
      nativeCheckInputs = [];
      nativeInstallCheckInputs = [];
      propagatedNativeBuildInputs = [];
    });
  binutilsMinimal = staticSet:
    (staticSet.binutils-unwrapped.override {
      enableGold = false;
      enableShared = false;
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      separateDebugInfo = false;
      nativeBuildInputs = (old.nativeBuildInputs or []) ++ [staticSet.buildPackages.pkg-config];
      buildInputs = [
        staticSet.zlib
        staticSet.zstd
      ];
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--enable-targets=all"
          "--disable-gas"
          "--disable-ld"
          "--disable-gprof"
          "--disable-nls"
          "--disable-plugins"
          "--without-debuginfod"
          "--with-zstd"
          "--prefix=/usr"
          "--libdir=/usr/lib"
          "--datadir=/usr/share"
          "--with-separate-debug-dir=/usr/lib/debug"
        ];
      buildPhase = ''
        runHook preBuild
        make -j"$NIX_BUILD_CORES" "''${makeFlags[@]}" \
          "TARGET-binutils=readelf objdump" all-binutils
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 binutils/readelf binutils/objdump -t "$out/bin"
        ${targetStrip staticSet} -s "$out/bin/readelf" "$out/bin/objdump"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      doCheck = false;
      doInstallCheck = false;
    });
  binutilsPackages = buildPkgs.lib.genAttrs linuxSystems (
    target: binutilsMinimal (staticSetFor target (pkgsFor target))
  );
in {
  binwalk = mkNixpkgsBinary {
    name = "binwalk";
    synopsis = "Analyze firmware images and embedded files";
    systems = portableSystems;
    packageFor = binwalkPackageFor;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libiconv.2.dylib"];
  };

  readelf = mkNixpkgsBinary {
    name = "readelf";
    synopsis = "Inspect ELF headers, sections, symbols, and debug information";
    systems = linuxSystems;
    packageFor = target: _: binutilsPackages.${target};
  };

  objdump = mkNixpkgsBinary {
    name = "objdump";
    synopsis = "Disassemble binaries and inspect object-file contents";
    systems = linuxSystems;
    packageFor = target: _: binutilsPackages.${target};
  };

  bpftool = mkNixpkgsBinary {
    name = "bpftool";
    synopsis = "Inspect and manage Linux eBPF objects";
    systems = linuxSystems;
    packageFor = system: _: bpftoolPackage system;
  };

  dbus-monitor = mkNixpkgsBinary {
    name = "dbus-monitor";
    synopsis = "Monitor messages on D-Bus buses";
    systems = linuxSystems;
    packageFor = target: targetPkgs: dbusMonitorMinimal (staticSetFor target targetPkgs);
  };

  strace = mkNixpkgsBinary {
    name = "strace";
    synopsis = "Trace Linux system calls and signals";
    systems = linuxSystems;
    packageFor = target: targetPkgs: (staticSetFor target targetPkgs).strace;
  };
  ltrace = mkNixpkgsBinary {
    name = "ltrace";
    synopsis = "Trace Linux library calls";
    systems = linuxSystems;
    packageFor = target: targetPkgs: ltraceMinimal (staticSetFor target targetPkgs) target;
  };

  gdb = mkNixpkgsBinary {
    name = "gdb";
    synopsis = "Debug native programs and inspect process state";
    systems = linuxSystems;
    packageFor = target: targetPkgs: gdbMinimal (staticSetFor target targetPkgs);
    rewriteBuildConfigurationPaths = true;
  };

  gdbserver = mkNixpkgsBinary {
    name = "gdbserver";
    synopsis = "Expose native programs to a remote GDB debugger";
    systems = linuxSystems;
    packageFor = target: targetPkgs: gdbMinimal (staticSetFor target targetPkgs);
    rewriteBuildConfigurationPaths = true;
  };

  pspy = mkNixpkgsBinary {
    name = "pspy";
    synopsis = "Monitor Linux processes without root permissions";
    systems = linuxSystems;
    packageFor = target: targetPkgs: (staticSetFor target targetPkgs).pspy;
  };
}
