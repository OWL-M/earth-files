{
  description = "Development shell for earth-files";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            just
            pkg-config
            rustPlatform.bindgenHook
          ];

          buildInputs = with pkgs; [
            glib # the `gio`/`glib` crates behind the gvfs mounter
            libxkbcommon # keymaps, via exwlshellev's waycrate_xkbkeycode
            wayland # the protocol layer: exwlshell and our own ui::dnd
            vulkan-loader # wgpu's Vulkan backend
          ];

          # These libraries are loaded with dlopen() at runtime. As `libcosmicAppHook`
          # did, embed an rpath through RUSTFLAGS so `./target/debug/earth-files` works
          # from an ordinary shell as well as `nix develop`. Using only LD_LIBRARY_PATH
          # causes `ConnectError(NoWaylandLib)` outside the development shell.
          shellHook =
            let
              libs = pkgs.lib.makeLibraryPath (
                with pkgs;
                [
                  vulkan-loader
                  libxkbcommon
                  wayland
                ]
              );
            in
            ''
              export LD_LIBRARY_PATH="${libs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
              export RUSTFLAGS="-C link-arg=-Wl,-rpath,${libs} ''${RUSTFLAGS:-}"
            '';
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
