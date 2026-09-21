{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.earth-files;
in
{
  options.programs.earth-files = {
    enable = lib.mkEnableOption "Earth Files";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.earth-files;
      defaultText = lib.literalExpression "inputs.earth-files.packages.\${pkgs.stdenv.hostPlatform.system}.earth-files";
      description = "Earth Files package to install.";
    };
    portal = {
      enable = lib.mkEnableOption ''
        the Earth Files file chooser backend for xdg-desktop-portal.

        Registers the backend and hands it
        `org.freedesktop.impl.portal.FileChooser` under `common`, forced so
        another module's default cannot quietly undo it.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      { environment.systemPackages = [ cfg.package ]; }
      (lib.mkIf cfg.portal.enable {
        xdg.portal = {
          enable = true;
          extraPortals = [ cfg.package ];
          config = {
            common."org.freedesktop.impl.portal.FileChooser" = lib.mkForce [
              "earthfiles"
            ];
          };
        };
      })
    ]
  );
}
