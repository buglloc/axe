{
  mkNixpkgsBinary,
  portableSystems,
  packageSetFor,
  staticSetFor,
  goDarwin,
  ...
}: {
  sqlite3 = mkNixpkgsBinary {
    name = "sqlite3";
    synopsis = "Query and modify SQLite databases";
    systems = portableSystems;
    packageFor = system: pkgs: (staticSetFor system pkgs).sqlite;
    rewriteBuildConfigurationPaths = true;
    darwinSystemLibraries = ["libz.dylib"];
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
