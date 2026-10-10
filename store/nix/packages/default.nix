{
  nixpkgs,
  additionalCaBundle ? null,
  extraCategories ? (_: {}),
}: let
  lib = nixpkgs.lib;
  helpers = import ./lib.nix {inherit nixpkgs additionalCaBundle;};
  publicCategories = {
    archives = import ./archives.nix helpers;
    containers = import ./containers.nix helpers;
    debugging = import ./debugging.nix helpers;
    files = import ./files.nix helpers;
    network = import ./network.nix helpers;
    runtime = import ./runtime.nix helpers;
    security = import ./security.nix helpers;
    storage = import ./storage.nix helpers;
    terminal = import ./terminal.nix helpers;
    text = import ./text.nix helpers;
  };
  extras = extraCategories helpers;
  categories =
    lib.mapAttrs (
      category: packages:
        packages // (extras.${category} or {})
    )
    publicCategories
    // lib.filterAttrs (category: _: !(builtins.hasAttr category publicCategories)) extras;
in
  helpers.mkPackageSet categories
