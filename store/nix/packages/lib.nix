{
  nixpkgs,
  additionalCaBundle ? null,
}: let
  lib = nixpkgs.lib;
  pkgsFor = system: import nixpkgs {inherit system;};

  buildPkgs = pkgsFor "x86_64-linux";
  axeOpenSslCaBundle = buildPkgs.runCommand "axe-openssl-ca-bundle.pem" {} ''
    cat ${buildPkgs.cacert}/etc/ssl/certs/ca-bundle.crt > "$out"
    ${lib.optionalString (additionalCaBundle != null) ''
      printf '\n' >> "$out"
      cat ${additionalCaBundle} >> "$out"
    ''}
  '';
  axePortableCaBundle = buildPkgs.runCommand "axe-portable-ca-bundle.pem" {} ''
    cat ${buildPkgs.cacert}/etc/ssl/certs/ca-no-trust-rules-bundle.crt > "$out"
    ${lib.optionalString (additionalCaBundle != null) ''
      printf '\n' >> "$out"
      cat ${additionalCaBundle} >> "$out"
    ''}
  '';

  linuxSystems = [
    "aarch64-linux"
    "x86_64-linux"
  ];
  darwinSystems = ["aarch64-darwin"];
  portableSystems = darwinSystems ++ linuxSystems;

  # aarch64-linux lacks native static package sets; use the cross musl set.
  staticSetFor = target: targetPkgs:
    if target == "aarch64-linux"
    then buildPkgs.pkgsCross.aarch64-multiplatform-musl.pkgsStatic
    else targetPkgs.pkgsStatic;

  # The complete Darwin static set is not consistently evaluable. Packages
  # that need static third-party libraries opt into staticSetFor directly.
  # Everything else uses the regular package set and macOS system libraries.
  packageSetFor = target: targetPkgs:
    if target == "aarch64-darwin"
    then targetPkgs
    else staticSetFor target targetPkgs;

  targetStrip = packageSet: "${packageSet.stdenv.cc.targetPrefix}strip";

  # Build pure-Go Darwin binaries on the Linux builder. Starting from the
  # Linux package keeps the derivation's build system x86_64-linux; changing
  # GOOS/GOARCH on a native Darwin derivation would still require a Mac.
  goDarwin = {
    package,
    subPackage,
    binary ? package,
  }:
    buildPkgs.${package}.overrideAttrs (old: {
      env =
        (old.env or {})
        // {
          CGO_ENABLED = 0;
          GOOS = "darwin";
          GOARCH = "arm64";
        };
      outputs = ["out"];
      buildPhase = ''
        runHook preBuild
        export GOCACHE="$TMPDIR/go-cache"
        export GOTOOLCHAIN=local
        # A same-named source directory makes go build -o write inside it.
        go build -buildmode=exe -trimpath \
          -ldflags "-linkmode internal ${toString (old.ldflags or [])}" \
          -o "$TMPDIR/${binary}" ${subPackage}
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 "$TMPDIR/${binary}" "$out/bin/${binary}"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      dontStrip = true;
      doCheck = false;
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  disableCdncheckIPv6Probe = package:
    package.overrideAttrs (old: {
      postConfigure =
        (old.postConfigure or "")
        + ''
          if [[ ! -d vendor/github.com/projectdiscovery/cdncheck && "''${GOPROXY:-}" == file://* ]]; then
            go mod vendor
            export GOFLAGS="$GOFLAGS -mod=vendor"
          fi
          if [[ -d vendor/github.com/projectdiscovery/cdncheck ]]; then
            chmod -R u+w vendor/github.com/projectdiscovery/cdncheck
            patch -d vendor/github.com/projectdiscovery/cdncheck -p1 \
              < ${../patches/cdncheck-disable-ipv6-probe.patch}
          fi
        '';
    });

  mkSingleBinary = {
    name,
    synopsis,
    system,
    package,
    aliases ? [],
  }: {
    inherit name aliases synopsis;
    channels.stable = package.version;
    artifact = {
      type = "single_binary";
      path = "bin/${name}";
    };
    targets.${system} = package;
  };

  mkNixpkgsBinary = {
    name,
    synopsis,
    systems,
    packageFor,
    aliases ? [],
    rewriteBuildConfigurationPaths ? false,
    darwinSystemLibraries ? [],
  }: let
    targetPackages = lib.genAttrs systems (
      system: let
        targetPkgs = pkgsFor system;
        package = packageFor system targetPkgs;
        executable = targetPkgs.lib.getExe' package name;
      in
        buildPkgs.runCommand "${name}-${package.version}"
        {
          inherit (package) version;
          nativeBuildInputs = [buildPkgs.perl];
        }
        ''
          install -Dm755 ${executable} "$out/bin/${name}"
          perl -0pi -e 's{(/nix/store/[0-9a-z]{32}-[^/\x00]+)/(etc/(?:mime\.types|protocols|services))}{"/" x (length($1) + 1) . $2}ge' "$out/bin/${name}"
          perl -0pi -e 's{(/nix/store/[0-9a-z]{32}-[^/\x00]+)/share/zoneinfo}{"/" x (length($1) - 3) . "usr/share/zoneinfo"}ge' "$out/bin/${name}"
          # Keep Mach-O offsets stable while remapping SDK-backed libraries to macOS.
          ${lib.optionalString (system == "aarch64-darwin") (
            lib.concatMapStringsSep "\n" (library: ''
              perl -0pi -e 's{(/nix/store/[0-9a-z]{32}-[^/\x00]+/lib/\Q${library}\E)(?=\x00)}{my $replacement = "/usr/lib/${library}"; $replacement . "\x00" x (length($1) - length($replacement))}ge' "$out/bin/${name}"
            '')
            darwinSystemLibraries
          )}
          ${lib.optionalString rewriteBuildConfigurationPaths ''
            # Some tools expose Nix configure arguments only as diagnostic text.
            # Keep the binary size stable while making that text host-independent.
            perl -0pi -e 's{/nix/store/}{/usr/local/}g' "$out/bin/${name}"
          ''}
          if grep -aFq /nix/store/ "$out/bin/${name}"; then
            echo "${name}: packaged binary still references /nix/store" >&2
            exit 1
          fi
        ''
    );
    versions = lib.unique (map (system: targetPackages.${system}.version) systems);
  in
    assert lib.assertMsg (systems != []) "Nixpkgs binary ${name} must have at least one target";
    assert lib.assertMsg (
      builtins.length versions == 1
    ) "Nixpkgs binary ${name} must have one version across all targets"; {
      inherit name aliases synopsis;
      channels.stable = builtins.head versions;
      artifact = {
        type = "single_binary";
        path = "bin/${name}";
      };
      targets = targetPackages;
    };

  mkUpstreamPackage = {
    name,
    version,
    system,
    url,
    hash,
    buildSystem ? system,
  }: let
    pkgs = pkgsFor buildSystem;
  in
    pkgs.stdenvNoCC.mkDerivation {
      pname = name;
      inherit version;

      src = pkgs.fetchurl {
        inherit url hash;
      };

      dontUnpack = true;
      installPhase = ''
        runHook preInstall
        install -Dm755 "$src" "$out/bin/${name}"
        runHook postInstall
      '';
    };

  mkUpstreamBinary = {
    name,
    version,
    synopsis,
    system,
    url,
    hash,
    aliases ? [],
  }:
    mkSingleBinary {
      inherit
        name
        synopsis
        system
        aliases
        ;
      package = mkUpstreamPackage {
        inherit
          name
          version
          system
          url
          hash
          ;
      };
    };

  mkUpstreamBinaries = {
    name,
    version,
    synopsis,
    sources,
    aliases ? [],
  }: {
    inherit
      name
      aliases
      synopsis
      ;
    channels.stable = version;
    artifact = {
      type = "single_binary";
      path = "bin/${name}";
    };
    targets =
      lib.mapAttrs (
        system: source:
          mkUpstreamPackage (
            {
              inherit name version system;
            }
            // source
          )
      )
      sources;
  };

  mkNixpkgsPackage = {
    name,
    synopsis,
    systems,
    packageFor,
    entrypoint,
    aliases ? [],
  }: let
    targetPackages = lib.genAttrs systems (system: packageFor system (pkgsFor system));
    versions = lib.unique (map (system: targetPackages.${system}.version) systems);
  in
    assert lib.assertMsg (systems != []) "Nixpkgs package ${name} must have at least one target";
    assert lib.assertMsg (
      builtins.length versions == 1
    ) "Nixpkgs package ${name} must have one version across all targets"; {
      inherit name aliases synopsis;
      channels.stable = builtins.head versions;
      artifact = {
        type = "package";
        inherit entrypoint;
      };
      targets = targetPackages;
    };

  mkPackageSet = categories: let
    categoryDefinitions = lib.concatLists (
      lib.mapAttrsToList (
        category: packages:
          lib.mapAttrsToList (attribute: definition: {
            inherit attribute;
            value =
              definition
              // {
                id = "${category}/${definition.name}";
              };
          })
          packages
      )
      categories
    );
    attributes = map ({attribute, ...}: attribute) categoryDefinitions;
    definitions = assert lib.assertMsg (
      builtins.length attributes == builtins.length (lib.unique attributes)
    ) "AXE Store package attributes must be globally unique";
      builtins.listToAttrs (
        map ({
          attribute,
          value,
        }:
          lib.nameValuePair attribute value)
        categoryDefinitions
      );
    systems = lib.unique (
      lib.concatMap (definition: builtins.attrNames definition.targets) (builtins.attrValues definitions)
    );
  in {
    metadata =
      lib.mapAttrs (
        _: definition:
          (removeAttrs definition ["targets"]) // {targets = builtins.attrNames definition.targets;}
      )
      definitions;

    packages = lib.genAttrs systems (
      system:
        lib.mapAttrs (_: definition: definition.targets.${system}) (
          lib.filterAttrs (_: definition: builtins.hasAttr system definition.targets) definitions
        )
    );
  };
in {
  inherit
    buildPkgs
    axeOpenSslCaBundle
    axePortableCaBundle
    linuxSystems
    darwinSystems
    portableSystems
    staticSetFor
    packageSetFor
    targetStrip
    goDarwin
    disableCdncheckIPv6Probe
    mkPackageSet
    mkSingleBinary
    mkNixpkgsBinary
    mkNixpkgsPackage
    mkUpstreamBinary
    mkUpstreamBinaries
    pkgsFor
    ;
}
