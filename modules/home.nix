# homeModules.default: `tab-keeper` on PATH and the native messaging host for each
# Chrome-family browser. The extension itself comes from nixosModules.default, because
# Chrome policy is machine-wide.
{
  self,
  inputs,
  lib,
  ...
}: let
  hostName = "io.github.tomasz_feliksik.tab_keeper";
  module = {
    config,
    lib,
    pkgs,
    ...
  }: let
    cfg = config.tabKeeper;
    # Chrome passes the extension origin as the only argument, and a manifest's `path`
    # cannot carry `host`, so this has to be a file of its own.
    wrapper = pkgs.writeShellScript "tab-keeper-host" ''
      exec ${lib.getExe cfg.package} host "$@"
    '';
    # Exactly one origin. A second would let another extension reach every tab.
    hostManifest = builtins.toJSON {
      name = hostName;
      description = "tab-keeper native messaging relay";
      path = wrapper;
      type = "stdio";
      allowed_origins = ["chrome-extension://${self.lib.extensionId}/"];
    };
    manifestFile = browser: {
      name = "${config.xdg.configHome}/${browser}/NativeMessagingHosts/${hostName}.json";
      value.text = hostManifest;
    };
  in {
    key = "tab-keeper/homeModules/default";

    options.tabKeeper = {
      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        defaultText = lib.literalExpression "tab-keeper's own package";
        description = "The `tab-keeper` package to install.";
      };
      browsers = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = ["google-chrome" "google-chrome-beta" "google-chrome-unstable" "chromium"];
        description = "Browser directories under the XDG config home that get the native host.";
      };
    };

    config = {
      home.packages = [cfg.package];
      # Linux only: the macOS location has not been measured.
      home.file = lib.mkIf pkgs.stdenv.hostPlatform.isLinux (lib.listToAttrs (map manifestFile cfg.browsers));
    };
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
    manifests = map (name: "${home.config.home-files}/.config/${name}/NativeMessagingHosts/${hostName}.json") [
      "google-chrome"
      "chromium"
    ];
  in {
    # Asserts the behaviour: a working command on PATH, and a manifest naming exactly one
    # extension and a wrapper that really runs the relay.
    checks.home-module = pkgs.runCommand "home-module-check" {nativeBuildInputs = [pkgs.jq];} (''
        ${home.config.home.path}/bin/tab-keeper --version
      ''
      + lib.concatMapStrings (manifest:
        if pkgs.stdenv.hostPlatform.isLinux
        then ''
          test "$(jq -r .name ${manifest})" = ${hostName}
          test "$(jq -r .type ${manifest})" = stdio
          test "$(jq -c .allowed_origins ${manifest})" = '["chrome-extension://${self.lib.extensionId}/"]'
          grep -q ' host "\$@"$' "$(jq -r .path ${manifest})"
          test -x "$(jq -r .path ${manifest})"
        ''
        else "")
      manifests
      + ''
        touch "$out"
      '');
  };
}
