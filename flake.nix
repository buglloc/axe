{
  description = "AXE shell, binary releases, and AXE Store package set";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {nixpkgs, ...}: let
    lib = nixpkgs.lib;
    axeStore = {
      mkPackageSet = arguments:
        import ./store/nix/packages ({inherit nixpkgs;} // arguments);
    };
    packageSet = axeStore.mkPackageSet {};
    axeRelease = {
      mkPackages = releaseMetadata:
        lib.mapAttrs (
          system: release: let
            axe = import ./nix/axe.nix {
              pkgs = import nixpkgs {inherit system;};
              inherit release;
            };
          in {
            inherit axe;
            default = axe;
          }
        )
        releaseMetadata;
    };
    axeReleases = builtins.fromJSON (builtins.readFile ./nix/axe-releases.json);
    releasePackages = axeRelease.mkPackages axeReleases;
    releaseSystems = builtins.attrNames releasePackages;
    packageSystems = lib.unique (
      (builtins.attrNames packageSet.packages) ++ releaseSystems
    );
    supportedSystems = [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-linux"
    ];
  in {
    packages = lib.genAttrs packageSystems (
      system:
        (packageSet.packages.${system} or {})
        // (releasePackages.${system} or {})
    );
    devShells = lib.genAttrs supportedSystems (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          config.allowUnfreePredicate = package: lib.getName package == "yandex-cloud";
        };
        cargoZigbuild = pkgs.cargo-zigbuild.overrideAttrs (old: {
          patches =
            (old.patches or [])
            ++ [
              (pkgs.fetchpatch {
                url = "https://github.com/rust-cross/cargo-zigbuild/commit/110abf59ba07cb84ed71b31c8cd81eefdc37bcec.patch";
                hash = "sha256-CTmasj5u7EqWuJaZKbuxP/Vq4IYJJZKpHMPqa+EWM80=";
              })
            ];
          postPatch =
            (old.postPatch or "")
            + ''
              substituteInPlace src/zig/wrapper.rs \
                --replace-fail "/bin/sh" "${pkgs.runtimeShell}"
            '';
        });
        zigCompiler = name: command: target:
          pkgs.writeShellScriptBin name ''
            args=()
            for arg in "$@"; do
              case "$arg" in
                --target=*) ;;
                *) args+=("$arg") ;;
              esac
            done
            exec ${pkgs.zig}/bin/zig ${command} -target ${target} "''${args[@]}"
          '';
        zigCcAarch64 = zigCompiler "zig-cc-aarch64-linux-musl" "cc" "aarch64-linux-musl";
        zigCxxAarch64 = zigCompiler "zig-cxx-aarch64-linux-musl" "c++" "aarch64-linux-musl";
        zigCcX86_64 = zigCompiler "zig-cc-x86_64-linux-musl" "cc" "x86_64-linux-musl";
        zigCxxX86_64 = zigCompiler "zig-cxx-x86_64-linux-musl" "c++" "x86_64-linux-musl";
        referenceTools = with pkgs;
          [
            binutils
            bzip2
            coreutils
            file
            findutils
            gnutar
            gzip
            jq
            xz
          ]
          ++ lib.optionals stdenv.hostPlatform.isLinux [
            iproute2
            util-linux
          ];
      in {
        default = pkgs.mkShell {
          packages = with pkgs;
            [
              alejandra
              clang
              cargoZigbuild
              cmake
              curl
              dnsutils
              file
              gnutar
              just
              hugo
              openssh
              pkg-config
              python3
              rust-analyzer
              rustup
              yandex-cloud
              zstd
              zig
            ]
            ++ referenceTools;
          CC_aarch64_unknown_linux_musl = "${zigCcAarch64}/bin/zig-cc-aarch64-linux-musl";
          CXX_aarch64_unknown_linux_musl = "${zigCxxAarch64}/bin/zig-cxx-aarch64-linux-musl";
          CC_x86_64_unknown_linux_musl = "${zigCcX86_64}/bin/zig-cc-x86_64-linux-musl";
          CXX_x86_64_unknown_linux_musl = "${zigCxxX86_64}/bin/zig-cxx-x86_64-linux-musl";
          AXE_REFERENCE_PATH = lib.makeBinPath referenceTools;
        };
      }
    );
    lib = {
      axeStoreMetadata = packageSet.metadata;
      inherit axeRelease axeStore;
    };
  };
}
