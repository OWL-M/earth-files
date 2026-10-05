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
  # The file chooser backend for xdg-desktop-portal. `earthfiles` is the name
  # the desktop's portal configuration refers to, taken from the basename of
  # the `.portal` file below.
  portalBusName = "org.freedesktop.impl.portal.desktop.earthfiles";
  portalUnit = "earth-files-portal.service";
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
    lockFile = ../Cargo.lock;
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
    "--bin"
    "earth-files-portal"
    "--bin"
    "earth-files-helper"
  ];
  cargoTestFlags = [ "--lib" ];

  preCheck = ''
    export XDG_DATA_DIRS="${shared-mime-info}/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
  '';

  postInstall = ''
    install -Dm644 target/xdgen/${appId}.desktop \
      "$out/share/applications/${appId}.desktop"
    install -Dm644 target/xdgen/${appId}.metainfo.xml \
      "$out/share/metainfo/${appId}.metainfo.xml"
    cp -r res/icons "$out/share/icons"
    desktop-file-validate "$out/share/applications/${appId}.desktop"

    # Lets the helper do a copy, move or delete's steps as root through
    # pkexec, with a prompt of its own; polkit finds it in the system
    # profile, where the NixOS module puts this package.
    mkdir -p "$out/share/polkit-1/actions"
    substitute res/${appId}.helper.policy \
      "$out/share/polkit-1/actions/${appId}.helper.policy" \
      --replace-fail @HELPER@ "$out/bin/earth-files-helper"

    # The portal's own class, so compositors and docks find an icon for its
    # file chooser windows; hidden from launchers.
    install -Dm644 res/com.owlm.EarthFilesPortal.desktop \
      "$out/share/applications/com.owlm.EarthFilesPortal.desktop"
    desktop-file-validate "$out/share/applications/com.owlm.EarthFilesPortal.desktop"

    # The xdg-desktop-portal file chooser backend. Three files, matching what
    # every other backend ships: what the bus should start, what systemd
    # should run, and which interfaces this backend answers for.
    install -Dm644 /dev/stdin \
      "$out/share/dbus-1/services/${portalBusName}.service" <<EOF
    [D-BUS Service]
    Name=${portalBusName}
    Exec=$out/bin/earth-files-portal
    SystemdService=${portalUnit}
    EOF

    install -Dm644 /dev/stdin "$out/share/systemd/user/${portalUnit}" <<EOF
    [Unit]
    Description=Portal service (Earth Files file chooser)
    PartOf=graphical-session.target
    After=graphical-session.target

    [Service]
    Type=dbus
    BusName=${portalBusName}
    ExecStart=$out/bin/earth-files-portal
    EOF

    install -Dm644 /dev/stdin \
      "$out/share/xdg-desktop-portal/portals/earthfiles.portal" <<EOF
    [portal]
    DBusName=${portalBusName}
    Interfaces=org.freedesktop.impl.portal.FileChooser;
    EOF
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
