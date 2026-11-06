# The signed extension, its update manifest and the NixOS module that installs them. Signing
# happens outside any build, because a private key must never reach the store: this only
# reads the committed package, and `checks.extension` fails when it lags its source.
{
  self,
  lib,
  ...
}: let
  extensionId = "niglgkgdkfompjmcbpldgepicfnjlfnc";
  # Reading a file from this flake's own source, so no import-from-derivation.
  manifest = builtins.fromJSON (builtins.readFile ../extension/manifest.json);

  nixosModule = {
    config,
    pkgs,
    ...
  }: let
    cfg = config.tabKeeper.extension;
    package = self.packages.${pkgs.stdenv.hostPlatform.system}.extension;
    # `normal_installed` lets a person disable nothing and remove nothing, and
    # `override_update_url` is what lets a file:// manifest stand in for the Web Store.
    policy = builtins.toJSON {
      ExtensionSettings.${extensionId} = {
        installation_mode = "normal_installed";
        update_url = "file://${package}/updates.xml";
        override_update_url = true;
      };
    };
    policyFile = directory: {
      name = "${directory}/tab-keeper.json";
      value.text = policy;
    };
  in {
    key = "tab-keeper/nixosModules/default";

    options.tabKeeper.extension = {
      enable = lib.mkEnableOption "installing the tab-keeper extension through Chrome policy";
      policyDirectories = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = ["opt/chrome/policies/managed" "chromium/policies/managed"];
        description = ''
          Managed policy directories under /etc. Chrome has no per-user policy on Linux,
          so this reaches every OS user of the machine; a user without the native host
          from `homeModules.default` gets an extension that answers nothing.
        '';
      };
    };

    config = lib.mkIf cfg.enable {
      environment.etc = lib.listToAttrs (map policyFile cfg.policyDirectories);
    };
  };
in {
  flake.nixosModules.default = nixosModule;

  # The one place the extension's ID is written down for Nix.
  flake.lib.extensionId = extensionId;

  perSystem = {
    pkgs,
    self',
    ...
  }: let
    source = lib.fileset.toSource {
      root = ../extension;
      fileset = lib.fileset.unions [
        ../extension/background.js
        ../extension/core.js
        ../extension/manifest.json
        ../extension/options.html
        ../extension/options.js
        ../extension/tab-keeper.crx
        ../extension/tests
      ];
    };
  in {
    packages.extension =
      pkgs.runCommand "tab-keeper-extension-${manifest.version}" {
        version = manifest.version;
      } ''
        mkdir "$out"
        cp ${source}/tab-keeper.crx "$out/tab-keeper.crx"
        cat > "$out/updates.xml" <<XML
        <?xml version='1.0' encoding='UTF-8'?>
        <gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>
          <app appid='${extensionId}'>
            <updatecheck codebase='file://$out/tab-keeper.crx' version='${manifest.version}'/>
          </app>
        </gupdate>
        XML
      '';

    checks = {
      # The behaviour tests, the permission list and the stale-package test.
      extension = pkgs.runCommand "extension-check" {nativeBuildInputs = [pkgs.nodejs];} ''
        cp -r ${source} extension
        chmod -R u+w extension
        cd extension
        node --test tests/*.test.mjs
        touch "$out"
      '';

      # The update manifest has to name the package it sits beside.
      extension-package = pkgs.runCommand "extension-package-check" {} ''
        grep -F "appid='${extensionId}'" ${self'.packages.extension}/updates.xml
        grep -F "codebase='file://${self'.packages.extension}/tab-keeper.crx'" ${self'.packages.extension}/updates.xml
        grep -F "version='${manifest.version}'" ${self'.packages.extension}/updates.xml
        test -s ${self'.packages.extension}/tab-keeper.crx
        touch "$out"
      '';
    };
  };
}
