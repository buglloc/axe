{
  mkNixpkgsBinary,
  portableSystems,
  packageSetFor,
  staticSetFor,
  goDarwin,
  ...
} @ helpers: {
  micro = import ./text/micro.nix helpers;
  ox = import ./text/ox.nix helpers;
  vim = import ./text/vim.nix helpers;

  sqlite3 = mkNixpkgsBinary {
    name = "sqlite3";
    synopsis = "Query and modify SQLite databases";
    systems = portableSystems;
    packageFor = system: pkgs: (staticSetFor system pkgs).sqlite;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libz.dylib"];
  };

  difft = mkNixpkgsBinary {
    name = "difft";
    synopsis = "Compare files with syntax-aware structural diffs";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libiconv.2.dylib"];
    packageFor = target: targetPkgs:
      if target == "aarch64-darwin"
      then
        (packageSetFor target targetPkgs).difftastic.overrideAttrs (old: {
          patches = (old.patches or []) ++ [../patches/difftastic-no-jemalloc-darwin.patch];
        })
      else (packageSetFor target targetPkgs).difftastic;
  };

  jq = mkNixpkgsBinary {
    name = "jq";
    synopsis = "Process and transform JSON data";
    systems = portableSystems;
    rewriteBuildConfigurationPaths = true;
    packageFor = system: pkgs: (staticSetFor system pkgs).jq;
  };

  yq = mkNixpkgsBinary {
    name = "yq";
    synopsis = "Process and transform YAML, JSON, and XML data";
    systems = portableSystems;
    packageFor = system: pkgs:
      if system == "aarch64-darwin"
      then
        goDarwin {
          package = "yq-go";
          binary = "yq";
          subPackage = ".";
        }
      else (packageSetFor system pkgs).yq-go;
  };
}
