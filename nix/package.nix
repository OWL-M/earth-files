{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsNoGuiHook,
  desktop-file-utils,
  glib,
  gvfs,
  gsettings-desktop-schemas,
  adwaita-icon-theme,
  hicolor-icon-theme,
  shared-mime-info,
  libxkbcommon,
  wayland,
  vulkan-loader,
  xdg-utils,
}:
let
  manifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  appId = "com.owlm.EarthFiles";
  runtimeLibraries = [
    libxkbcommon
    wayland
    vulkan-loader
  ];
in
rustPlatform.buildRustPackage rec {
  pname = "earth-files";
  version = manifest.package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../i18n.toml
      ../src
      ../res
      ../i18n
      ../examples
      ../LICENSE
    ];
  };

  cargoLock = {
    lockFile = "${src}/Cargo.lock";
    allowBuiltinFetchGit = true;
  };

  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
    wrapGAppsNoGuiHook
    desktop-file-utils
  ];
  buildInputs = [
    glib
    gvfs
    gsettings-desktop-schemas
    adwaita-icon-theme
    hicolor-icon-theme
    shared-mime-info
  ]
  ++ runtimeLibraries;

  cargoBuildFlags = [
    "--bin"
    "earth-files"
  ];
  cargoTestFlags = [ "--lib" ];

  postInstall = ''
    install -Dm644 target/xdgen/${appId}.desktop \
      "$out/share/applications/${appId}.desktop"
    install -Dm644 target/xdgen/${appId}.metainfo.xml \
      "$out/share/metainfo/${appId}.metainfo.xml"
    cp -r res/icons "$out/share/icons"
    desktop-file-validate "$out/share/applications/${appId}.desktop"
  '';

  preFixup = ''
    gappsWrapperArgs+=(
      --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibraries}"
      --prefix XDG_DATA_DIRS : "${
        lib.makeSearchPath "share" [
          adwaita-icon-theme
          hicolor-icon-theme
          shared-mime-info
        ]
      }"
      --prefix PATH : "$out/bin:${
        lib.makeBinPath [
          glib
          xdg-utils
        ]
      }"
    )
  '';

  meta = {
    description = "Standalone Wayland file manager";
    homepage = "https://github.com/owl-m/earth-files";
    license = lib.licenses.gpl3Only;
    mainProgram = "earth-files";
    platforms = lib.platforms.linux;
  };
}
