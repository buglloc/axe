{
  mkNixpkgsBinary,
  linuxSystems,
  staticSetFor,
  ...
}:
mkNixpkgsBinary {
  name = "broot";
  synopsis = "Browse directory trees with fuzzy search and file previews";
  systems = linuxSystems;
  packageFor = system: pkgs:
    (staticSetFor system pkgs).broot.overrideAttrs (old: {
      patches =
        (old.patches or [])
        ++ [
          ../../patches/broot-disable-shell-install-prompt.patch
          ../../patches/broot-read-only-startup.patch
        ];
      buildInputs = [];
      env = builtins.removeAttrs (old.env or {}) ["RUSTONIG_SYSTEM_LIBONIG"];
      postPatch = ''
        substituteInPlace Cargo.toml \
          --replace-fail 'build = "build.rs"' 'build = false'
      '';
      postInstall = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });
}
