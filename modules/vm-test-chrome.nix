# The whole install on branded Google Chrome, which is what people run and which the other
# tests cannot use: it ignores --load-extension, so policy is the only way in. Chrome is
# unfree and x86_64 only, and the test takes minutes, so it is a package to run on purpose
# (`just vm-test-chrome`), and nothing else in this flake depends on it.
{
  self,
  inputs,
  lib,
  ...
}: {
  perSystem = {
    pkgs,
    system,
    ...
  }: let
    chrome =
      (import inputs.nixpkgs {
        inherit system;
        config.allowUnfreePredicate = package: lib.getName package == "google-chrome";
      }).google-chrome;
    # A page holding a known amount of memory, so that unloading it can be measured.
    pages = pkgs.runCommand "tab-keeper-test-pages" {} ''
      mkdir "$out"
      cat > "$out/heavy.html" <<'HTML'
      <!doctype html><title>heavy</title><script>
        window.held = [];
        for (let i = 0; i < 400; i++) window.held.push(new Uint8Array(1 << 20).fill(i % 251 + 1));
      </script>
      HTML
      for page in light exempt other reading later; do
        echo "<!doctype html><title>$page</title>" > "$out/$page.html"
      done
    '';
    # Everything Chrome runs as, by proportional share, so shared memory is not counted twice.
    chromeMemory = pkgs.writeShellScript "chrome-memory-kib" ''
      total=0
      for status in /proc/[0-9]*; do
        case "$(readlink "$status/exe" 2>/dev/null)" in
          *chrome*)
            pss=$(${pkgs.gawk}/bin/awk '/^Pss:/ {print $2}' "$status/smaps_rollup" 2>/dev/null || true)
            total=$((total + ''${pss:-0}))
            ;;
        esac
      done
      echo "$total"
    '';
  in {
    packages = lib.optionalAttrs (system == "x86_64-linux") {
      vm-test-chrome = pkgs.testers.runNixOSTest {
        name = "tab-keeper-branded-chrome";
        skipTypeCheck = true;
        globalTimeout = 1200;
        nodes.machine = {
          imports = [
            self.nixosModules.default
            inputs.home-manager.nixosModules.home-manager
          ];
          tabKeeper.extension.enable = true;
          environment.systemPackages = [chrome pkgs.xorg-server];
          environment.etc."opt/chrome/policies/managed/exceptions.json".text = builtins.toJSON {
            TabDiscardingExceptions = ["127.0.0.1"];
          };
          users.users.person = {
            isNormalUser = true;
            uid = 1000;
          };
          virtualisation = {
            memorySize = 4096;
            cores = 2;
          };
          home-manager.users.person = {
            imports = [self.homeModules.default];
            home.stateVersion = "25.11";
          };
          systemd.tmpfiles.rules = ["d /run/user/1000 0700 person users -"];
        };
        testScript = ''
          import json, shlex, time

          ENVIRONMENT = "XDG_RUNTIME_DIR=/run/user/1000 DISPLAY=:99 "

          def as_person(command):
              return "su person -c " + shlex.quote(ENVIRONMENT + command)

          def keeper_status(*arguments):
              command = "tab-keeper --json " + " ".join(map(shlex.quote, arguments))
              status, output = machine.execute(as_person(command))
              return status, json.loads(output)

          def keeper(*arguments):
              status, answer = keeper_status(*arguments)
              assert status == 0, f"{arguments}: {status} {answer}"
              return answer

          def chrome(profile, url):
              machine.succeed(as_person(
                  "google-chrome-stable --no-sandbox --disable-gpu --no-first-run "
                  + "--password-store=basic --profile-directory=" + shlex.quote(profile) + " "
                  + shlex.quote(url) + " >>/tmp/chrome.log 2>&1 &"
              ))

          def listing():
              return keeper("list", "--limit", "1000", "--full")

          def tab_at(path, listed=None):
              found = [tab for tab in (listed or listing())["tabs"] if path in tab["url"]]
              return found[0] if found else None

          def wait_for_tab(path):
              machine.wait_until_succeeds(
                  as_person("tab-keeper --json list --full --limit 1000") + " | grep -q " + shlex.quote(path),
                  timeout=180,
              )

          def memory_kib():
              return int(machine.succeed("${chromeMemory}").strip())

          machine.wait_for_unit("multi-user.target")
          machine.wait_for_unit("home-manager-person.service")
          machine.succeed("${pkgs.python3}/bin/python3 -m http.server 8000 --directory ${pages} >/tmp/pages.log 2>&1 &")
          machine.succeed(as_person("Xvfb :99 -screen 0 1280x800x24 >/tmp/xvfb.log 2>&1 &"))
          machine.wait_for_file("/tmp/.X11-unix/X99")

          print("VERSION", machine.succeed("google-chrome-stable --version").strip())

          with subtest("the policy installs the signed package and the host connects"):
              machine.fail(as_person("tab-keeper list"))
              chrome("Default", "http://localhost:8000/light.html")
              wait_for_tab("light.html")
              assert listing()["partial"] is False

          with subtest("discard frees real memory and the tab stays"):
              chrome("Default", "http://localhost:8000/heavy.html")
              wait_for_tab("heavy.html")
              # Let it allocate, then put another tab in front so it is no longer active.
              time.sleep(15)
              chrome("Default", "http://localhost:8000/other.html")
              wait_for_tab("other.html")
              time.sleep(5)
              heavy = tab_at("heavy.html")
              assert not heavy.get("active"), heavy
              before = memory_kib()
              answer = keeper("discard", heavy["ref"])
              print("DISCARD", json.dumps(answer))
              assert answer["results"][0]["status"] == "done", answer
              time.sleep(10)
              after = memory_kib()
              print("MEMORY_KIB before", before, "after", after, "freed", before - after)
              unloaded = tab_at("heavy.html")
              assert unloaded.get("discarded") is True, unloaded
              # Chrome 156 replaces the ID here, and the answer has to say so.
              print("REFERENCE before", heavy["ref"], "after", unloaded["ref"])
              assert answer["results"][0]["new_ref"] == unloaded["ref"], answer
              assert before - after > 200 * 1024, "at least half of what the page holds"

          with subtest("the active tab is refused, and the machine's exemption policy holds"):
              active = [tab for tab in listing()["tabs"] if tab.get("active")][0]
              status, answer = keeper_status("discard", active["ref"])
              print("ACTIVE", status, json.dumps(answer))
              assert status == 2 and answer["results"][0]["status"] == "refused", answer
              chrome("Default", "http://127.0.0.1:8000/exempt.html")
              wait_for_tab("exempt.html")
              chrome("Default", "http://localhost:8000/later.html")
              wait_for_tab("later.html")
              time.sleep(2)
              status, answer = keeper_status("discard", tab_at("exempt.html")["ref"])
              print("EXEMPT", status, json.dumps(answer))
              assert status == 2 and answer["results"][0]["status"] == "refused", answer
              assert tab_at("exempt.html").get("discarded") is not True

          with subtest("a move is reported as it really landed"):
              moved = keeper("move", tab_at("light.html")["ref"], "--index", "-1")
              print("MOVE", json.dumps(moved))
              assert moved["results"][0]["status"] == "done", moved

          with subtest("close and restore keep the group"):
              grouped = keeper(
                  "group", tab_at("light.html")["ref"], tab_at("exempt.html")["ref"],
                  "--title", "work", "--color", "red",
              )
              print("GROUPED", json.dumps(grouped))
              closed = keeper("close", tab_at("light.html")["ref"], tab_at("exempt.html")["ref"])
              print("CLOSED", json.dumps(closed))
              assert tab_at("light.html") is None
              restored = keeper("restore", closed["records"][0])
              print("RESTORED", json.dumps(restored))
              wait_for_tab("light.html")
              wait_for_tab("exempt.html")
              back = listing()
              assert tab_at("light.html", back).get("group") == "work", back
              assert tab_at("exempt.html", back).get("group") == "work", back

          with subtest("a removed bookmark comes back"):
              added = keeper("bookmark", "add", "--title", "a mark", "--url", "http://localhost:8000/light.html")
              print("BOOKMARK", json.dumps(added))
              marks = keeper("bookmark", "list", "--limit", "200")["bookmarks"]
              mark = [b for b in marks if b["title"] == "a mark"][0]
              removed = keeper("bookmark", "remove", mark["id"])
              marks = keeper("bookmark", "list", "--limit", "200")["bookmarks"]
              assert not [b for b in marks if b["title"] == "a mark"]
              keeper("bookmark", "restore", removed["record"])
              marks = keeper("bookmark", "list", "--limit", "200")["bookmarks"]
              assert [b for b in marks if b["title"] == "a mark"]

          with subtest("a second profile in the same browser answers on its own"):
              chrome("Profile 1", "http://localhost:8000/reading.html")
              wait_for_tab("reading.html")
              listed = listing()
              print("PROFILES", json.dumps(listed["profiles"]))
              assert len(listed["profiles"]) == 2, listed["profiles"]
              assert tab_at("reading.html", listed)["profile"] != tab_at("heavy.html", listed)["profile"]

          with subtest("a saved group survives restarting the browser"):
              profile = tab_at("light.html")["profile"]
              saved = keeper("group", "save", "keep", "--group", "work", "--profile", profile)
              print("SAVED", json.dumps(saved))
              assert tab_at("light.html") is None
              # Only the browser process, as a window manager or logout would ask. Signalling
              # every process at once crashes it, and a new start then hands its URL to the
              # dying browser and exits, leaving nothing running.
              lock = "/home/person/.config/google-chrome/SingletonLock"
              browser = machine.succeed(f"readlink {lock}").strip().rsplit("-", 1)[1]
              machine.succeed(f"kill -TERM {browser}")
              machine.wait_until_fails(f"test -d /proc/{browser}", timeout=60)
              machine.wait_until_fails("pgrep -u person chrome", timeout=60)
              machine.wait_until_fails("pgrep -u person -x tab-keeper", timeout=60)
              machine.fail(as_person("tab-keeper list"))
              chrome("Default", "http://localhost:8000/other.html")
              wait_for_tab("other.html")
              opened = keeper("group", "open", "keep")
              print("OPENED", json.dumps(opened))
              wait_for_tab("light.html")
              wait_for_tab("exempt.html")
              back = listing()
              assert tab_at("light.html", back).get("group") == "work", back
              assert [g for g in keeper("group", "list")["groups"] if g["name"] == "keep"][0]["color"] == "red"

          with subtest("nothing here crashed the browser"):
              status, dumps = machine.execute("coredumpctl list --no-pager")
              print("COREDUMPS", status, dumps)
              assert status != 0, dumps
        '';
      };
    };
  };
}
