# Explicit binary repackaging, NOT a reproducible Rust/Skia source derivation.
# The caller supplies an immutable staged release directory from scripts/stage-linux.py.
{ pkgs, artifactRoot }:
let
  libraries = with pkgs; [ stdenv.cc.cc.lib openssl dbus fontconfig freetype libGL
    wayland libxkbcommon libx11 libxcursor libxi libxrandr libxcb ];
in pkgs.stdenvNoCC.mkDerivation {
  pname = "peersus";
  version = "0.1.0";
  src = artifactRoot;
  nativeBuildInputs = [ pkgs.autoPatchelfHook pkgs.makeWrapper ];
  buildInputs = libraries;
  dontConfigure = true;
  dontBuild = true;
  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -a bin share "$out/"
    chmod -R u+w "$out"
    wrapProgram "$out/bin/peersus-ui" --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath libraries}" --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.zenity ]}
    wrapProgram "$out/bin/peersus" --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.zenity ]}
    runHook postInstall
  '';
  # autoPatchelf fails for unresolved ELF dependencies. This still does not prove desktop runtime.
  meta = {
    description = "Peersus prebuilt release repackaged with Nix runtime dependencies";
    license = pkgs.lib.licenses.asl20;
    platforms = pkgs.lib.platforms.linux;
    mainProgram = "peersus-ui";
  };
}
