{ pkgs, sourceRoot, skiaPins }:
let
  rustEnvironment = pkgs.rust.override {
    stdenv = pkgs.clangStdenv;
    pkgsBuildHost = pkgs.buildPackages // { stdenv = pkgs.clangStdenv; };
  };
  rustPlatform = (pkgs.makeRustPlatform {
    inherit (pkgs) rustc cargo;
    stdenv = pkgs.clangStdenv;
  }).overrideScope (_: _: pkgs.callPackages
    (pkgs.path + "/pkgs/build-support/rust/hooks") {
      stdenv = pkgs.clangStdenv;
      rust = rustEnvironment;
    });
  manifestFile = name:
    let directory = sourceRoot + ("/crates/" + name);
    in if builtins.pathExists (directory + "/plugin.json") then "plugin.json" else "plugin.toml";
  readManifest = name:
    let file = manifestFile name;
        text = builtins.readFile (sourceRoot + ("/crates/" + name + "/" + file));
    in if file == "plugin.json" then builtins.fromJSON text else builtins.fromTOML text;
  pluginCrates = builtins.filter (name:
    builtins.pathExists (sourceRoot + ("/crates/" + name + "/plugin.json")) ||
    builtins.pathExists (sourceRoot + ("/crates/" + name + "/plugin.toml"))
  ) (builtins.attrNames (builtins.readDir (sourceRoot + "/crates")));
  linuxPluginCrates = builtins.filter (name:
    let platforms = (readManifest name).platforms or null;
    in platforms == null || builtins.elem "linux" platforms
  ) pluginCrates;
  pins = builtins.fromJSON (builtins.readFile skiaPins);
  skiaArchive = pkgs.fetchurl {
    url = "https://codeload.github.com/rust-skia/skia/tar.gz/61e7ca4e99062cdd0ab69445d5963fb3365778f6";
    sha256 = "8401d0de878582a03abc1029bb62330aea58cda89904ae6ab187302e6d48ba5d";
  };
  skiaSource = pkgs.runCommand "peersus-skia-source" { nativeBuildInputs = [ pkgs.gnutar pkgs.gzip ]; } ''
    mkdir -p "$out"
    tar -xzf ${skiaArchive} --strip-components=1 -C "$out"
  '';
  externals = pkgs.lib.mapAttrs (_: pin: pkgs.fetchzip {
    url = pin.archive;
    inherit (pin) hash;
    stripRoot = false;
  }) pins.externals;
in rustPlatform.buildRustPackage {
  pname = "peersus";
  version = (builtins.fromTOML (builtins.readFile "${sourceRoot}/Cargo.toml")).workspace.package.version;
  src = pkgs.lib.fileset.toSource {
    root = sourceRoot;
    fileset = pkgs.lib.fileset.unions (map (name: sourceRoot + ("/" + name))
      [ "Cargo.toml" "Cargo.lock" "build-support" "crates" "vendor" "xtask" ".cargo" "LICENSE" "NOTICE" "packaging" ]);
  };
  cargoLock.lockFile = "${sourceRoot}/Cargo.lock";
  cargoBuildFlags = [ "-p" "peersus-ui" "-p" "peersus-app" "-p" "peersus-plugin-runtime" "-p" "peersus-store-fetch" ] ++ pkgs.lib.concatMap (name: [ "-p" name ]) linuxPluginCrates;
  # Runtime and full workspace tests belong to cargo xtask gate, not this GUI package builder.
  doCheck = false;
  # Ninja/CMake are Skia build tools, not the workspace's top-level build system.
  dontUseNinjaBuild = true;
  dontUseNinjaInstall = true;
  dontUseCmakeConfigure = true;
  nativeBuildInputs = with pkgs; [ pkg-config rustPlatform.bindgenHook llvmPackages.lld python3 gn ninja cmake makeWrapper ];
  buildInputs = with pkgs; [ openssl dbus fontconfig freetype libGL wayland libxkbcommon
    libx11 libxcursor libxi libxrandr libxcb ];
  FORCE_SKIA_BUILD = "1";
  SKIA_GN_COMMAND = "${pkgs.gn}/bin/gn";
  SKIA_NINJA_COMMAND = "${pkgs.ninja}/bin/ninja";
  preBuild = ''
    source=${skiaSource}
    test "$(sha256sum "$source/DEPS" | cut -d ' ' -f1)" = '${pins.depsSha256}'
    cp -R "$source" "$TMPDIR/peersus-skia"
    chmod -R u+w "$TMPDIR/peersus-skia"
    export SKIA_SOURCE_DIR="$TMPDIR/peersus-skia"
    mkdir -p "$SKIA_SOURCE_DIR/third_party/externals"
    ${pkgs.lib.concatStringsSep "\n" (pkgs.lib.mapAttrsToList (name: source: ''
      ln -s ${source} "$SKIA_SOURCE_DIR/${name}"
    '') externals)}
  '';
  postInstall = ''
    $CC -std=c11 -O2 -Wall -Wextra -Werror -fstack-protector-strong -fPIE -pie \
      crates/peersus-plugin-runtime/native/host.c -Wl,-z,relro,-z,now -ldl -o "$out/bin/peersus-native-host"
    ${pkgs.lib.concatMapStringsSep "\n" (name:
      let manifest = readManifest name;
          directory = pkgs.lib.removePrefix "peersus-" name;
      in pkgs.lib.concatMapStringsSep "\n" (program: ''
        install -Dm755 "$out/bin/${program}" "$out/share/peersus/plugins/${directory}/${program}"
      '') ([ manifest.program ] ++ (manifest.files or [])) + ''
        install -Dm644 crates/${name}/${manifestFile name} "$out/share/peersus/plugins/${directory}/${manifestFile name}"
      ''
    ) linuxPluginCrates}
    for host in peersus-ui peersus; do
      wrapProgram "$out/bin/$host" --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.zenity ]}
    done
    install -Dm644 packaging/linux/io.github.DarkPhilosophy.Peersus.desktop "$out/share/applications/io.github.DarkPhilosophy.Peersus.desktop"
    install -Dm644 packaging/linux/io.github.DarkPhilosophy.Peersus.metainfo.xml "$out/share/metainfo/io.github.DarkPhilosophy.Peersus.metainfo.xml"
    install -Dm644 crates/peersus-ui/assets/peersus-icon.svg "$out/share/icons/hicolor/scalable/apps/io.github.DarkPhilosophy.Peersus.svg"
  '';
  meta = { license = pkgs.lib.licenses.asl20; platforms = pkgs.lib.platforms.linux; mainProgram = "peersus-ui"; };
}
