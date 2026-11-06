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
| `modules/package.nix`   | The crate through crane and the fenix `stable` toolchain, Clippy check, dev shells  |
| `modules/formatter.nix` | One formatting entry point, `nix fmt`: alejandra, rustfmt, prettier                 |
| `modules/home.nix`      | `homeModules.default` and a check that it puts a working command on `PATH`          |
| `justfile`              | The recipes CI and people both run                                                  |
| `.github/workflows/`    | Format, lint, test, e2e, and signing on `main` and tags, with actions pinned        |
| `modules/extension.nix` | The signed extension, its update manifest, `nixosModules.default` and their checks  |
| `modules/vm-test.nix`   | The whole install on a fresh NixOS VM, run on purpose with `just vm-test`           |
| `extension/`            | The extension's source, `pack.sh`, the signed package and the encrypted key         |
| `tests/e2e/`            | The guarantees against real Chromium, in the `e2e` dev shell                        |

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

Then import `inputs.tab-keeper.homeModules.default` into a Home Manager configuration, for the command and the native
host, and `inputs.tab-keeper.nixosModules.default` into the NixOS one with `tabKeeper.extension.enable = true`, because
Chrome policy is machine-wide. Check with `chrome://policy` that the policy was accepted, never by reading the file.

## 5. The end-to-end tests

`just e2e` runs in CI on every push. It costs a 2.2 GiB download of Playwright's Chromium, once per runner, and about 11
seconds for the tests, and it is the only place that catches what Chrome itself adjusts or refuses. It lives in its own
dev shell so that nothing else pays for that download. `just vm-test` needs KVM and is run on purpose.

## 6. Signing the extension

Needs an age identity the key is encrypted to, either your own or `SOPS_AGE_KEY`. Run this after any change to the
extension's files, and commit the result:

```bash
nix develop .#e2e --command just pack
```

The first pack was bootstrapped once, by generating the key, encrypting it with `sops` under the rule in `.sops.yaml`,
and putting its public half in the manifest's `key`. That never needs repeating: replacing the key changes the extension
ID.
