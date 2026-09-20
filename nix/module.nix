{
  self,
  isHome ? false,
}:
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
  };

  config = lib.mkIf cfg.enable (
    if isHome then
      {
        home.packages = [ cfg.package ];
      }
    else
      {
        environment.systemPackages = [ cfg.package ];
      }
  );
}
