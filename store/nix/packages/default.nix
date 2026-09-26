{
  nixpkgs,
  additionalCaBundle ? null,
  extraCategories ? (_: {}),
}: let
  lib = nixpkgs.lib;
  helpers = import ./lib.nix {inherit nixpkgs additionalCaBundle;};
  publicCategories = {
    containers = import ./containers.nix helpers;
    data = import ./data.nix helpers;
    debugging = import ./debugging.nix helpers;
    network = import ./network.nix helpers;
    runtime = import ./runtime.nix helpers;
    search = import ./search.nix helpers;
    security = import ./security.nix helpers;
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
