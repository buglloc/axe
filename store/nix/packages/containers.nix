{
  mkNixpkgsBinary,
  buildPkgs,
  linuxSystems,
  portableSystems,
  staticSetFor,
  packageSetFor,
  goDarwin,
  targetStrip,
  ...
} @ helpers: let
  utilLinuxMinimal = system: pkgs: (staticSetFor system pkgs).util-linuxMinimal;

  bubblewrapMinimal = staticSet:
    staticSet.bubblewrap.overrideAttrs (old: {
      outputs = ["out"];
      nativeBuildInputs = [
        buildPkgs.meson
        buildPkgs.ninja
        buildPkgs.pkg-config
        buildPkgs.python3
      ];
      buildInputs = [
        (staticSet.libcap.override {runtimeShell = "/bin/sh";})
        staticSet.libselinux
      ];
      mesonFlags =
        (old.mesonFlags or [])
        ++ [
          "--prefix=/usr"
          "-Ddefault_library=static"
          "-Dc_link_args=-static"
          "-Dman=disabled"
          "-Dbash_completion=disabled"
          "-Dzsh_completion=disabled"
          "-Dtests=false"
        ];
      buildPhase = ''
        runHook preBuild
        ninja bwrap
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 bwrap "$out/bin/bwrap"
        ${targetStrip staticSet} -s "$out/bin/bwrap"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
    });

  dockerMinimal = system: packageSet:
    (packageSet.docker-client.override {
      buildxSupport = false;
      composeSupport = false;
      clientOnly = true;
      glibc =
        packageSet.emptyDirectory
        // {
          static = null;
        };
    }).overrideAttrs
    (old: {
      outputs = ["out"];
      buildInputs = [];
      env.CGO_ENABLED = 0;
      buildPhase = ''
        runHook preBuild
        export GOCACHE="$TMPDIR/go-cache"
        export CGO_ENABLED=0
        export GO111MODULE=auto
        export GOOS=${
          if system == "aarch64-darwin"
          then "darwin"
          else "linux"
        }
        export GOARCH=${
          if system == "aarch64-darwin" || system == "aarch64-linux"
          then "arm64"
          else "amd64"
        }
        mkdir -p .gopath/src/github.com/docker
        ln -s "$PWD" .gopath/src/github.com/docker/cli
        export GOPATH="$PWD/.gopath:$GOPATH"
        go build \
          -buildmode=exe \
          -o docker \
          -tags grpcnotrace \
          -ldflags '-buildid= -linkmode internal -s -w -X github.com/docker/cli/cli/version.Version=${old.version} -X github.com/docker/cli/cli/version.GitCommit=v${old.version} -X github.com/docker/cli/cli/version.BuildTime=1970-01-01T00:00:00Z' \
          github.com/docker/cli/cmd/docker
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 docker "$out/bin/docker"
        ${packageSet.buildPackages.removeReferencesTo}/bin/remove-references-to \
          -t ${packageSet.buildPackages.go} "$out/bin/docker"
        sed -i 's|/nix/store/|/usr/local/|g' "$out/bin/docker"
        runHook postInstall
      '';
      postFixup = "";
      doInstallCheck = false;
      nativeInstallCheckInputs = [];
    });

  kubectlMinimal = system: packageSet:
    packageSet.kubectl.overrideAttrs (old: {
      env.CGO_ENABLED = 0;
      outputs = ["out"];
      buildPhase = ''
        runHook preBuild
        export GOCACHE="$TMPDIR/go-cache"
        export GOTOOLCHAIN=local
        export CGO_ENABLED=0
        export GOARCH=${
          if system == "aarch64-darwin" || system == "aarch64-linux"
          then "arm64"
          else "amd64"
        }
        export GOOS=${
          if system == "aarch64-darwin"
          then "darwin"
          else "linux"
        }
        go build \
          -buildmode=exe \
          -mod=vendor \
          -trimpath \
          -tags ${
          if system == "aarch64-darwin"
          then "notest,grpcnotrace"
          else "selinux,notest,grpcnotrace"
        } \
          -ldflags '-linkmode internal -s -w -X k8s.io/client-go/pkg/version.gitVersion=v${old.version} -X k8s.io/component-base/version.gitVersion=v${old.version}' \
          -o kubectl \
          ./cmd/kubectl
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        install -Dm755 kubectl "$out/bin/kubectl"
        runHook postInstall
      '';
      postInstall = "";
      postFixup = "";
      dontStrip = true;
    });

  sternPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "stern";
          subPackage = ".";
        }
      else (packageSetFor target targetPkgs).stern
    ).overrideAttrs
    (old: {
      outputs = ["out"];
      env =
        (old.env or {})
        // {
          CGO_ENABLED = 0;
          GOFLAGS = (old.env.GOFLAGS or "") + " -tags=timetzdata";
        };
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      postInstall = "";
    });

  helmPackageFor = target: targetPkgs:
    (
      if target == "aarch64-darwin"
      then
        goDarwin {
          package = "kubernetes-helm";
          subPackage = "./cmd/helm";
          binary = "helm";
        }
      else (packageSetFor target targetPkgs).kubernetes-helm
    ).overrideAttrs
    (old: {
      outputs = ["out"];
      env = (old.env or {}) // {CGO_ENABLED = 0;};
      ldflags = (old.ldflags or []) ++ ["-linkmode=internal"];
      buildPhase =
        if target == "aarch64-darwin"
        then ''
          runHook preBuild
          export GOCACHE="$TMPDIR/go-cache"
          export GOTOOLCHAIN=local
          go build -buildmode=exe -trimpath \
            -ldflags "$ldflags" \
            -o "$TMPDIR/helm" ./cmd/helm
          runHook postBuild
        ''
        else old.buildPhase;
      postInstall = "";
    });

  podmanVersion = "6.1.2";
  podmanSources = {
    aarch64-darwin = {
      url = "https://github.com/podman-container-tools/podman/releases/download/v${podmanVersion}/podman-remote-release-darwin_arm64.zip";
      hash = "sha256-wZx63T36bV9C509LB9vsvBS2H9IecJL/090bpsr74qY=";
      path = "podman-${podmanVersion}/usr/bin/podman";
      archive = "zip";
    };
    aarch64-linux = {
      url = "https://github.com/podman-container-tools/podman/releases/download/v${podmanVersion}/podman-remote-static-linux_arm64.tar.gz";
      hash = "sha256-BFuo8PJNi8aPQ2xnwNzCNfFczxaXWWpOCkPMjTS6K+o=";
      path = "bin/podman-remote-static-linux_arm64";
      archive = "tar";
    };
    x86_64-linux = {
      url = "https://github.com/podman-container-tools/podman/releases/download/v${podmanVersion}/podman-remote-static-linux_amd64.tar.gz";
      hash = "sha256-Z4Xk3BHa1nAAMIdJ/tD5gWmHkjCYMKa4cL1al7NScYI=";
      path = "bin/podman-remote-static-linux_amd64";
      archive = "tar";
    };
  };
  podmanPackage = system: let
    source = podmanSources.${system};
    archive = buildPkgs.fetchurl {
      inherit (source) url hash;
    };
  in
    buildPkgs.runCommand "podman-${podmanVersion}-${system}"
    {
      version = podmanVersion;
      nativeBuildInputs = [
        buildPkgs.gnutar
        buildPkgs.gzip
        buildPkgs.unzip
      ];
    }
    ''
      ${
        if source.archive == "zip"
        then "unzip -q ${archive} ${source.path}"
        else "tar -xzf ${archive} ${source.path}"
      }
      install -Dm755 ${source.path} "$out/bin/podman"
    '';
in {
  cek = import ./containers/cek.nix helpers;
  layerx = import ./containers/layerx.nix helpers;

  bwrap = mkNixpkgsBinary {
    name = "bwrap";
    synopsis = "Run commands in isolated Linux namespaces";
    systems = linuxSystems;
    packageFor = target: targetPkgs: bubblewrapMinimal (staticSetFor target targetPkgs);
  };

  docker = mkNixpkgsBinary {
    name = "docker";
    synopsis = "Manage Docker containers through a remote daemon";
    systems = portableSystems;
    packageFor = system: pkgs: dockerMinimal system (packageSetFor system pkgs);
  };

  kubectl = mkNixpkgsBinary {
    name = "kubectl";
    synopsis = "Manage Kubernetes clusters from the command line";
    systems = portableSystems;
    packageFor = system: _: kubectlMinimal system buildPkgs.pkgsStatic;
    rewriteBuildConfigurationPaths = true;
  };

  stern = mkNixpkgsBinary {
    name = "stern";
    synopsis = "Tail logs from multiple Kubernetes pods and containers";
    systems = portableSystems;
    packageFor = sternPackageFor;
  };

  helm = mkNixpkgsBinary {
    name = "helm";
    synopsis = "Install and manage Kubernetes applications with Helm charts";
    systems = portableSystems;
    packageFor = helmPackageFor;
  };

  lsns = mkNixpkgsBinary {
    name = "lsns";
    synopsis = "List Linux namespaces and their member processes";
    systems = linuxSystems;
    packageFor = utilLinuxMinimal;
  };

  nsenter = mkNixpkgsBinary {
    name = "nsenter";
    synopsis = "Run a program in another process's Linux namespaces";
    systems = linuxSystems;
    packageFor = utilLinuxMinimal;
  };

  unshare = mkNixpkgsBinary {
    name = "unshare";
    synopsis = "Run a program in new Linux namespaces";
    systems = linuxSystems;
    packageFor = utilLinuxMinimal;
  };

  podman = mkNixpkgsBinary {
    name = "podman";
    synopsis = "Manage Podman through its remote service";
    systems = portableSystems;
    packageFor = system: _: podmanPackage system;
  };
}
