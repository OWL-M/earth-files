{
  description = "Earth Files, a standalone Wayland file manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default-linux";
  };

  outputs =
    {
      self,
      nixpkgs,
      systems,
      ...
    }:
    let
      eachSystem = nixpkgs.lib.genAttrs (import systems);
    in
    {
      packages = eachSystem (system: {
        earth-files = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
        default = self.packages.${system}.earth-files;
      });

      apps = eachSystem (system: {
        earth-files = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.earth-files;
          meta.description = "Earth Files";
        };
        default = self.apps.${system}.earth-files;
      });

      overlays.default = final: prev: {
        earth-files = final.callPackage ./nix/package.nix { };
      };

      nixosModules.earth-files = import ./nix/module.nix { inherit self; };
      nixosModules.default = self.nixosModules.earth-files;

      devShells = eachSystem (system: {
        default = nixpkgs.legacyPackages.${system}.callPackage ./nix/shell.nix {
          inherit (self.packages.${system}) earth-files;
        };
      });

      checks = eachSystem (system: {
        package = self.packages.${system}.earth-files;
      });

      formatter = eachSystem (system: nixpkgs.legacyPackages.${system}.nixfmt-tree);
    };
}
