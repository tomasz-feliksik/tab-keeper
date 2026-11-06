# homeModules.default: `tab-keeper` on PATH. The native messaging host manifest
# joins it when the relay exists (see the backlog).
{
  self,
  inputs,
  ...
}: let
  module = {
    config,
    lib,
    pkgs,
    ...
  }: let
    cfg = config.tabKeeper;
  in {
    key = "tab-keeper/homeModules/default";

    options.tabKeeper = {
      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        defaultText = lib.literalExpression "tab-keeper's own package";
        description = "The `tab-keeper` package to install.";
      };
    };

    config.home.packages = [cfg.package];
  };
in {
  flake.homeModules.default = module;

  perSystem = {pkgs, ...}: let
    home = inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home = {
            username = "test";
            homeDirectory = "/home/test";
            stateVersion = "25.11";
          };
        }
      ];
    };
  in {
    # Asserts the behaviour: the module puts a working command on the home's PATH.
    checks.home-module = pkgs.runCommand "home-module-check" {} ''
      ${home.config.home.path}/bin/tab-keeper --version
      touch "$out"
    '';
  };
}
