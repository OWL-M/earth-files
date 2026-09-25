{
  lib,
  mkShell,
  earth-files,
  rustPlatform,
  cargo,
  rustc,
  clippy,
  rustfmt,
  rust-analyzer,
  just,
  nixfmt-tree,
  typos,
  xdg-utils,
  libxkbcommon,
  wayland,
  vulkan-loader,
  gvfs,
  adwaita-icon-theme,
  hicolor-icon-theme,
  shared-mime-info,
}:
let
  runtimePath = lib.makeLibraryPath [
    vulkan-loader
    libxkbcommon
    wayland
  ];
in
mkShell {
  name = "earth-files-dev";
  inputsFrom = [ earth-files ];
  packages = [
    cargo
    rustc
    clippy
    rustfmt
    rust-analyzer
    just
    nixfmt-tree
    typos
    xdg-utils
  ];

  RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";

  shellHook = ''
    export GIO_EXTRA_MODULES="${gvfs}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"
    export XDG_DATA_DIRS="$PWD/res:${
      lib.makeSearchPath "share" [
        adwaita-icon-theme
        hicolor-icon-theme
        shared-mime-info
      ]
    }:''${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
    export LD_LIBRARY_PATH="${runtimePath}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    # Keep locally built binaries runnable outside the development shell.
    export RUSTFLAGS="-C link-arg=-Wl,-rpath,${runtimePath} ''${RUSTFLAGS:-}"
  '';
}
