# Upgrading the extension on branded Chrome, the way a machine gets a new tab-keeper: a
# system switch changes the policy's package, and Chrome has to end up running the new
# code. Three machines, each asking the running extension which version answers:
#
#   released  0.1.0 as released, to this release, restarting Chrome as often as it takes
#   restart   this release to a later one, restarting Chrome once
#   periodic  this release to a later one, never restarting
#
# Unfree, x86_64 only and minutes long, so a package to run on purpose (`just vm-test-chrome-upgrade`).
{
  self,
  inputs,
  lib,
  ...
}: {
  perSystem = {
    pkgs,
    self',
    system,
    ...
  }: let
    chrome =
      (import inputs.nixpkgs {
        inherit system;
        config.allowUnfreePredicate = package: lib.getName package == "google-chrome";
      }).google-chrome;
    inherit (self.lib) extensionId;
    current = (builtins.fromJSON (builtins.readFile ../extension/manifest.json)).version;

    # A signed package with its update manifest, as `packages.extension` is built.
    signed = version: crx:
      pkgs.runCommand "tab-keeper-extension-${version}" {} ''
        mkdir "$out"
        cp ${crx} "$out/tab-keeper.crx"
        cat > "$out/updates.xml" <<XML
        <?xml version='1.0' encoding='UTF-8'?>
        <gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>
          <app appid='${extensionId}'>
            <updatecheck codebase='file://$out/tab-keeper.crx' version='${version}'/>
          </app>
        </gupdate>
        XML
      '';
    # What machines ran before this release: 0.1.0 exactly as released.
    released = signed "0.1.0" ../tests/vm/tab-keeper-0.1.0.crx;
    # This source as a later release, written by `just pack` (extension/pack.sh).
    next = signed "99.0.0" ../tests/vm/tab-keeper-next.crx;

    page = pkgs.writeTextDir "light.html" "<!doctype html><title>light</title>";

    machine = from: to: {
      imports = [
        self.nixosModules.default
        inputs.home-manager.nixosModules.home-manager
      ];
      tabKeeper.extension = {
        enable = true;
        package = lib.mkDefault from;
      };
      # What `nixos-rebuild switch` to the new release does to the policy.
      specialisation.upgraded.configuration.tabKeeper.extension.package = to;
      environment.systemPackages = [chrome pkgs.xorg-server];
      users.users.person = {
        isNormalUser = true;
        uid = 1000;
      };
      virtualisation = {
        memorySize = 2048;
        cores = 2;
      };
      home-manager.users.person = {
        imports = [self.homeModules.default];
        home.stateVersion = "25.11";
      };
      systemd.tmpfiles.rules = ["d /run/user/1000 0700 person users -"];
    };
  in {
    packages = lib.optionalAttrs (system == "x86_64-linux") {
      vm-test-chrome-upgrade = pkgs.testers.runNixOSTest {
        name = "tab-keeper-branded-chrome-upgrade";
        skipTypeCheck = true;
        globalTimeout = 1800;
        nodes = {
          released = machine released self'.packages.extension;
          restart = machine self'.packages.extension next;
          periodic = machine self'.packages.extension next;
        };
        testScript = {nodes, ...}: ''
          import json, shlex, threading, time

          ENVIRONMENT = "XDG_RUNTIME_DIR=/run/user/1000 DISPLAY=:99 "
          PROFILE = "/home/person/.config/google-chrome/Default"
          CURRENT = "${current}"
          NEXT = "99.0.0"

          def as_person(command):
              return "su person -c " + shlex.quote(ENVIRONMENT + command)

          def start_chrome(m, flags=""):
              m.succeed(as_person(
                  "google-chrome-stable --no-sandbox --disable-gpu --no-first-run --password-store=basic "
                  + flags + " http://localhost:8000/light.html >>/tmp/chrome.log 2>&1 &"
              ))
              m.wait_until_succeeds(as_person("tab-keeper --json list") + " | grep -q light.html", timeout=180)

          def stop_chrome(m):
              browser = m.succeed(f"readlink {PROFILE}/../SingletonLock").strip().rsplit("-", 1)[1]
              m.succeed(f"kill -TERM {browser}")
              m.wait_until_fails("pgrep -u person chrome", timeout=60)

          def running(m):
              """The version the running extension reports, None if it is older than 0.1.1."""
              status, output = m.execute(as_person("tab-keeper --json profiles"))
              if status != 0:
                  return "unreachable"
              return json.loads(output)["profiles"][0]["extension_version"]

          def on_disk(m):
              """Versions Chrome has unpacked. Its Preferences lag behind, so they are not used."""
              status, output = m.execute(f"ls {PROFILE}/Extensions/${extensionId}")
              return output.split() if status == 0 else []

          def wait_for_version(m, version, seconds):
              started = time.monotonic()
              while time.monotonic() - started < seconds:
                  if running(m) == version:
                      return int(time.monotonic() - started)
                  time.sleep(5)
              return None

          def report(m, what):
              print(m.name.upper(), what, "running", running(m), "on disk", on_disk(m))

          def prepare(m, flags=""):
              m.wait_for_unit("multi-user.target")
              m.wait_for_unit("home-manager-person.service")
              m.succeed("${pkgs.python3}/bin/python3 -m http.server 8000 --directory ${page} >/tmp/pages.log 2>&1 &")
              m.succeed(as_person("Xvfb :99 -screen 0 1280x800x24 >/tmp/xvfb.log 2>&1 &"))
              m.wait_for_file("/tmp/.X11-unix/X99")
              start_chrome(m, flags)

          def switch(m):
              node = {"released": "${nodes.released.system.build.toplevel}",
                      "restart": "${nodes.restart.system.build.toplevel}",
                      "periodic": "${nodes.periodic.system.build.toplevel}"}[m.name]
              m.succeed(f"{node}/specialisation/upgraded/bin/switch-to-configuration test")

          def released_upgrade():
              m = released
              prepare(m)
              assert running(m) is None, "0.1.0 reports no version"
              switch(m)
              # Chrome does not look for an update when the policy changes.
              assert wait_for_version(m, CURRENT, 120) is None, "measured table is stale: Chrome now updates on a policy change"
              report(m, "two minutes after the switch, no restart:")
              # 0.1.0 has no onUpdateAvailable listener, so Chrome downloads the update on
              # the first restart and holds it back while the extension runs.
              for attempt in (1, 2):
                  stop_chrome(m)
                  start_chrome(m)
                  took = wait_for_version(m, CURRENT, 120)
                  report(m, f"restart {attempt}, {took} s:")
                  if took is not None:
                      break
              assert running(m) == CURRENT, "the new release runs after at most two restarts"
              print("RELEASED needed", attempt, "restarts")

          def restart_upgrade():
              m = restart
              prepare(m)
              assert running(m) == CURRENT, running(m)
              switch(m)
              stop_chrome(m)
              start_chrome(m)
              took = wait_for_version(m, NEXT, 180)
              report(m, f"one restart, {took} s:")
              assert took is not None, "this release applies an update on the restart that downloads it"

          def periodic_upgrade():
              m = periodic
              # Chrome itself looks only every few hours (--extensions-update-frequency did
              # nothing in 10 minutes); the extension asks every 10 minutes.
              prepare(m)
              assert running(m) == CURRENT, running(m)
              switch(m)
              took = None
              for minute in range(15):
                  took = wait_for_version(m, NEXT, 60)
                  report(m, f"no restart, minute {minute + 1}:")
                  if took is not None:
                      break
              assert took is not None, "this release applies an update without a restart"

          print("VERSION", released.succeed("google-chrome-stable --version").strip())
          # The machines run side by side. The driver's subtest() is not thread-safe, so each
          # scenario reports for itself.
          start_all()
          failures = []
          def run(name, scenario):
              try:
                  scenario()
                  print("PASSED", name)
              except Exception as error:
                  print("FAILED", name, repr(error))
                  failures.append(f"{name}: {error!r}")
          threads = [
              threading.Thread(target=run, args=("0.1.0 to this release", released_upgrade)),
              threading.Thread(target=run, args=("this release to the next, one restart", restart_upgrade)),
              threading.Thread(target=run, args=("this release to the next, no restart", periodic_upgrade)),
          ]
          for thread in threads:
              thread.start()
          for thread in threads:
              thread.join()
          assert not failures, failures
        '';
      };
    };
  };
}
