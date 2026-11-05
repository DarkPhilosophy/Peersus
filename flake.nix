{
  description = "Peersus source package and pinned Linux development environment";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/1c89083e451e891152365b799595cf7790a43325";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      eachSystem = nixpkgs.lib.genAttrs systems;
    in {
      lib.mkBinaryPackage = { system, artifactRoot }: import ./packaging/nix/from-artifact.nix {
        pkgs = import nixpkgs { inherit system; };
        inherit artifactRoot;
      };
      packages = eachSystem (system: {
        default = import ./packaging/nix/source-package.nix {
          pkgs = import nixpkgs { inherit system; };
          sourceRoot = ./.;
          skiaPins = ./nix/skia-externals.json;
        };
      });
      devShells = eachSystem (system:
        let pkgs = import nixpkgs { inherit system; }; in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc rustfmt clippy cargo-audit cargo-ndk
              clang llvmPackages.libclang llvmPackages.llvm pkg-config cmake ninja
              python3 git curl fontconfig freetype openssl dbus libGL libxkbcommon
              wayland libx11 libxcursor libxi libxrandr libxcb ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            FORCE_SKIA_BUILD = "1";
            shellHook = ''
              echo "Source Skia builds require network access in this development shell."
              echo "The Nix package uses separately pinned offline Skia sources.";
            '';
          };
        });
    };
}
