# The whole install, from the flake's own modules, on a machine that has never seen it:
# real policy, real Chromium, real host manifest, and `tab-keeper list` at the end. A VM
# test needs KVM and minutes, so it is a package to run on purpose (`just vm-test`), not a
# flake check.
{
  self,
  inputs,
  lib,
  ...
}: {
  perSystem = {pkgs, ...}: {
    packages = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
      vm-test = pkgs.testers.runNixOSTest {
        name = "tab-keeper-install";
        nodes.machine = {
          imports = [
            self.nixosModules.default
            inputs.home-manager.nixosModules.home-manager
          ];
          tabKeeper.extension.enable = true;
          environment.systemPackages = [pkgs.chromium];
          environment.etc."chromium/policies/managed/exceptions.json".text = builtins.toJSON {
            TabDiscardingExceptions = ["meet.example.com"];
          };
          users.users.person = {
            isNormalUser = true;
            uid = 1000;
          };
          virtualisation.memorySize = 2048;
          home-manager.users.person = {
            imports = [self.homeModules.default];
            home.stateVersion = "25.11";
          };
          systemd.tmpfiles.rules = ["d /run/user/1000 0700 person users -"];
        };
        testScript = ''
          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("home-manager-person.service")

          as_person = "su person -c 'XDG_RUNTIME_DIR=/run/user/1000 %s'"

          # Nothing is running yet, and that has to be an error rather than an empty list.
          machine.fail(as_person % "tab-keeper list")

          machine.succeed(as_person % (
              "chromium --headless=new --no-sandbox --disable-gpu --no-first-run "
              + "--user-data-dir=/home/person/.config/chromium about:blank "
              + ">/tmp/chromium.log 2>&1 &"
          ))

          # Installing from policy, starting the worker and connecting takes a while.
          machine.wait_until_succeeds(as_person % "tab-keeper list", timeout=180)
          listed = machine.succeed(as_person % "tab-keeper --json list")
          print(listed)
          assert "about:blank" in listed

          # The policy that was read is the machine's own, and discard honours it.
          dry_run = machine.succeed(as_person % "tab-keeper --json discard --idle 0s --dry-run")
          print(dry_run)
          assert '"dry_run":true' in dry_run
        '';
      };
    };
  };
}
