{
  mkNixpkgsBinary,
  portableSystems,
  packageSetFor,
  targetStrip,
  ...
}:
mkNixpkgsBinary {
  name = "ox";
  synopsis = "Edit text in the terminal with Lua configuration and plugins";
  systems = portableSystems;
  rewriteBuildConfigurationPaths = true;
  darwinSystemLibraries = ["libiconv.2.dylib"];
  packageFor = system: pkgs: let
    packageSet = packageSetFor system pkgs;
  in
    if system == "aarch64-darwin"
    then packageSet.ox
    else
      packageSet.ox.overrideAttrs (old: {
        env =
          (old.env or {})
          // {
            RUSTFLAGS = (old.env.RUSTFLAGS or "") + " -C relocation-model=static";
          };
        postInstall =
          (old.postInstall or "")
          + ''
            ${targetStrip packageSet} -s "$out/bin/ox"
          '';
        doInstallCheck = false;
        postConfigure =
          (old.postConfigure or "")
          + ''
            patch -p1 < ${../../patches/ox-axe-defaults.patch}
          '';
        nativeInstallCheckInputs = [];
      });
}
