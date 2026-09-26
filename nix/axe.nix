{ pkgs, release }:

pkgs.stdenvNoCC.mkDerivation {
  pname = "axe";
  inherit (release) version;

  src = pkgs.fetchurl {
    inherit (release) url hash;
  };

  dontUnpack = true;

  installPhase = ''
    runHook preInstall
    install -Dm755 "$src" "$out/bin/axe"
    runHook postInstall
  '';

  passthru.stableUrl = release.stable_url;

  meta = {
    description = "Brush shell with statically bundled Unix applets";
    license = pkgs.lib.licenses.mit;
    mainProgram = "axe";
    platforms = [ pkgs.stdenv.hostPlatform.system ];
    sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
  };
}
