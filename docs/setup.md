# Setting up the repository

How this repository was created and how it is structured. It is written down so it can be repeated or checked.

## 1. Create the repository

Needs `gh`, authenticated, and `bd` 1.0 or newer.

```bash
git init -b main ~/code/projects/tab-keeper
cd ~/code/projects/tab-keeper
gh repo create tomasz-feliksik/tab-keeper --public \
  --description "Lets people and agents list, close, group and arrange Chrome tabs across profiles"
git remote add origin git@github.com:tomasz-feliksik/tab-keeper.git
```

Do not run the tracker setup from a linked worktree: `bd init` there walks up to `~/.beads` and builds the tracker in
the wrong place.

## 2. Set up the tracker

```bash
bd init --non-interactive
chmod 700 .beads
git config beads.role maintainer

# Not used here.
bd setup cursor --remove
git rm .claude/settings.json    # a second `bd prime` hook; the global one already covers it
git rm -r .codex                # the same, for Codex

bd dolt remote add origin git+https://github.com/tomasz-feliksik/tab-keeper.git --allow-git-origin
bd config set sync.remote "https://github.com/tomasz-feliksik/tab-keeper.git"
```

Issues sync with `bd dolt push`, which writes `refs/dolt/data` on the remote and also creates a `__dolt_remote_info__`
branch there. That branch is normal bookkeeping, and CI skips it.

**Push `main` before the first `bd dolt push`.** On an empty GitHub repository, the first branch to arrive becomes the
default.

Git pushes do not carry the tracker, so finishing a session takes two pushes: `bd dolt push` and `git push`.

## 3. The structure

| Path                    | What it is                                                                          |
| ----------------------- | ----------------------------------------------------------------------------------- |
| `flake.nix`             | Inputs and one line of outputs: every file under `modules/` is a flake-parts module |
| `modules/parts.nix`     | Supported systems and imported flake modules                                        |
| `modules/package.nix`   | The crate through crane and the fenix `stable` toolchain, Clippy check, dev shell   |
| `modules/formatter.nix` | One formatting entry point, `nix fmt`: alejandra, rustfmt, prettier                 |
| `modules/home.nix`      | `homeModules.default` and a check that it puts a working command on `PATH`          |
| `justfile`              | The recipes CI and people both run                                                  |
| `.github/workflows/`    | `format_check`, `lint`, `release_check`, `test`, with actions pinned by commit      |
| `reference/prototype/`  | The frozen prototype. Excluded from formatting and linting, deleted after the port  |

`.beads/`, `.agents/`, `AGENTS.md` and `CLAUDE.md` are excluded from formatting because `bd` regenerates parts of them.

## 4. Using it from another flake

```nix
{
  inputs.tab-keeper = {
    url = "github:tomasz-feliksik/tab-keeper";
    inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

Then import `inputs.tab-keeper.homeModules.default` into a Home Manager configuration. For now this only installs the
command. The extension policy needs a NixOS module, because Chrome policy is machine-wide. Both are part of M1.
