{
  mkNixpkgsPackage,
  buildPkgs,
  linuxSystems,
  packageSetFor,
  ...
}: let
  terminals = [
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

  ghosttyTerminfo =
    buildPkgs.runCommand "vim-ghostty-terminfo-${buildPkgs.ghostty.version}" {
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

  portableNcurses = packageSet:
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
          "$tic_path" -o $tmp_info -x "${ghosttyTerminfo}" >&2 || exit 1'
        '';
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--with-fallbacks=${buildPkgs.lib.concatStringsSep "," terminals}"
          "--with-tic-path=${buildPkgs.ncurses}/bin/tic"
          "--with-infocmp-path=${buildPkgs.ncurses}/bin/infocmp"
          "--with-default-terminfo-dir=/usr/share/terminfo"
          "--disable-db-install"
          "--without-tests"
        ];
    });

  portableVim = packageSet: let
    ncurses = portableNcurses packageSet;
  in
    (packageSet.vim.override {inherit ncurses;}).overrideAttrs (old: {
      outputs = ["out"];
      meta = old.meta // {outputsToInstall = ["out"];};
      patches = (old.patches or []) ++ [../../patches/vim-portable-runtime.patch];
      nativeBuildInputs = [buildPkgs.pkg-config];
      buildInputs = [ncurses packageSet.pixman];
      env =
        (old.env or {})
        // {
          CFLAGS = "-O2 -fno-pie";
          LDFLAGS = "-static -no-pie";
        };
      configureFlags =
        (old.configureFlags or [])
        ++ [
          "--prefix=/usr"
          "--with-features=huge"
          "--with-compiledby=AXE"
          "--with-tlib=ncurses"
          "--enable-gui=no"
          "--without-x"
          "--disable-wayland"
          "--disable-xsmp"
          "--disable-xim"
          "--disable-gpm"
          "--disable-acl"
          "--disable-nls"
          "--disable-canberra"
          "--disable-libsodium"
          "--disable-pythoninterp"
          "--disable-python3interp"
          "--disable-luainterp"
          "--disable-rubyinterp"
          "--disable-perlinterp"
          "--disable-tclinterp"
          "--disable-mzschemeinterp"
        ];
      buildPhase = ''
        runHook preBuild
        make -C src -j"$NIX_BUILD_CORES" vim
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        staged="$(mktemp -d)"
        make -C src installruntime DESTDIR="$staged"
        install -Dm755 src/vim "$out/bin/vim"
        mkdir -p "$out/share"
        cp -R "$staged/usr/share/vim" "$out/share/"
        rm -rf "$staged"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      propagatedBuildInputs = [];
      propagatedNativeBuildInputs = [];
      dontPatchShebangs = true;
    });
in
  mkNixpkgsPackage {
    name = "vim";
    synopsis = "Edit text with Vim and its standard runtime";
    systems = linuxSystems;
    entrypoint = "bin/vim";
    packageFor = system: pkgs: portableVim (packageSetFor system pkgs);
  }
