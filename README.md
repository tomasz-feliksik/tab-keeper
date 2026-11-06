# tab-keeper

Lets people and agents list, close, group and arrange Chrome tabs across every profile.

**Status: built, and tested on Chromium.** Every command and MCP tool exists and passes against real Chromium, and a
fresh NixOS VM installs it from this flake. It has not yet been run on branded Chrome on a real machine.

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

Restart Chrome, then run `tab-keeper list`. To use it from an agent, register `tab-keeper mcp` as a stdio MCP server,
and keep its write and destructive tools behind your harness's permission prompt.

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
```

Work is tracked with [beads](https://github.com/gastownhall/beads): run `bd ready` to see what can be done next.

## License

[MIT](./LICENSE)
