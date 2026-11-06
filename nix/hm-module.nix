# Home Manager module: programs.claude-code-sync.
# The CLI has no config file, so the module only installs the package.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.claude-code-sync;
  system = pkgs.stdenv.hostPlatform.system;
in
{
  options.programs.claude-code-sync = {
    enable = lib.mkEnableOption "claude-code-sync, a backup, restore and sync tool for ~/.claude";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.claude-code-sync;
      defaultText = lib.literalExpression "claude-code-sync.packages.\${system}.claude-code-sync";
      description = "The claude-code-sync package.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
  };
}
