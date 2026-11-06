# tab-keeper

Lets people and agents list, close, group and arrange Chrome tabs across every profile.

**Status: not usable yet.** Only the native messaging relay is built. A working prototype of `list` and `discard` is in
[`reference/prototype/`](./reference/prototype/), and finishing the Rust port is the first milestone.

## Why

Chrome has no tool that does all of this at once:

- sees every profile, not only one
- groups, moves and closes tabs, not only lists them
- connects to an agent over MCP
- does not give that agent control of the pages themselves

tab-keeper is a small browser extension and a local Rust program. The extension can see tab titles and URLs, but it
cannot read or change page contents. An agent proposes a tidy-up, you approve it, and every tab it closes is saved first
so it can be restored.

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
nix flake check                    # builds, Clippy, formatting, Home Manager module
```

Work is tracked with [beads](https://github.com/gastownhall/beads): run `bd ready` to see what can be done next.

## License

[MIT](./LICENSE)
