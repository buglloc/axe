{
  mkNixpkgsPackage,
  buildPkgs,
  linuxSystems,
  packageSetFor,
  portableNcurses,
  ...
}: let
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
