# tab-keeper

Lets people and agents list, close, group and arrange Chrome tabs across every profile.

**Status: built, and tested on Chromium and on branded Chrome in a VM.** Every command and MCP tool exists and passes
against real Chromium, and a fresh NixOS VM installs it from this flake and runs every kind of command on branded
Chrome 156. It is installed on one real machine with several running profiles, and has not yet seen days of use.

## Why

Chrome has no tool that does all of this at once:

- sees every profile, not only one
- groups, moves and closes tabs, not only lists them
- connects to an agent over MCP
- does not give that agent control of the pages themselves

tab-keeper is a small browser extension and a local Rust program. The extension can see tab titles and URLs, but it
cannot read or change page contents. An agent proposes a tidy-up, you approve it, and every tab it closes is saved first
so it can be restored.

## Installing it

With Nix, on Linux. The extension is installed by a machine-wide Chrome policy, and the command and native host belong
to each user. Add the flake as an input, then:

```nix
# NixOS
imports = [inputs.tab-keeper.nixosModules.default];
tabKeeper.extension.enable = true;

# Home Manager
imports = [inputs.tab-keeper.homeModules.default];
```

**If your configuration already sets `ExtensionSettings` in a Chrome policy file, do not use the NixOS module.** It
writes that policy from a file of its own, Chrome uses only one file's value for a policy, and an extension that stops
being listed gets uninstalled. Skip the module and add its entry to your existing list instead:

```nix
ExtensionSettings.${inputs.tab-keeper.lib.extensionId} = {
  installation_mode = "normal_installed";
  update_url = "file://${inputs.tab-keeper.packages.${pkgs.stdenv.hostPlatform.system}.extension}/updates.xml";
  override_update_url = true;
};
```

Then run `tab-keeper list`; a running Chrome picks the policy up without a restart. After an upgrade, the new extension
is running within about 10 minutes; `tab-keeper profiles` shows the version each profile runs. Coming from 0.1.0, quit
and start Chrome twice instead, with a minute between: the first start downloads the update, which can take most of that
minute, and the second loads it. The update has arrived once the profile's
`Extensions/niglgkgdkfompjmcbpldgepicfnjlfnc/` holds a `0.1.1_0` folder. If a tool of yours allows every
policy-installed extension in incognito, exclude this one, or agents will see those tabs. To use it from an agent,
register `tab-keeper mcp` as a stdio MCP server, and keep its write and destructive tools behind your harness's
permission prompt.

Titles and URLs are written by websites, and tab-keeper hands them to your agent labelled as untrusted data. A page can
still try to talk an agent into something, and the tab list can hold private URLs. **An agent that has this and a
network tool could be persuaded to send those URLs somewhere, and nothing in tab-keeper can prevent that.** Keep that
combination behind prompts you read.

## Documentation

| Document                                                             | What it covers                                 |
| -------------------------------------------------------------------- | ---------------------------------------------- |
| [`docs/project-overview.md`](./docs/project-overview.md)             | What this is, how it fits together, decisions  |
| [`docs/functionality.md`](./docs/functionality.md)                   | Commands, MCP tools and the guarantees         |
| [`docs/things-to-keep-in-mind.md`](./docs/things-to-keep-in-mind.md) | Chrome's limits, security, traps, alternatives |
| [`docs/setup.md`](./docs/setup.md)                                   | Repository, tracker and Nix structure          |

## Development

```bash
nix develop --command just fmt     # format
nix develop --command just check   # lint and test: the CI entry point
nix flake check                    # builds, Clippy, formatting, Home Manager module, extension
nix develop .#e2e --command just e2e   # the guarantees against real Chromium
just vm-test                       # the whole install on a fresh NixOS VM
just vm-test-chrome                # the same on branded Chrome, with every kind of command
```

Work is tracked with [beads](https://github.com/gastownhall/beads): run `bd ready` to see what can be done next.

## License

[MIT](./LICENSE)
