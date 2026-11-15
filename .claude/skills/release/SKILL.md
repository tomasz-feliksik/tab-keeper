---
name: release
description:
  Cut a tab-keeper release end to end — find the last release, diff every user-facing surface since (CLI, MCP tools, the
  extension, the socket protocol, saved records, the Nix modules, the guarantees), propose the version with the evidence
  shown, raise the version in the binary and the extension together, re-sign, run every gate including the VM tests CI
  cannot run, tag, and publish the GitHub release with the signed extension and curated notes. Use when asked to
  release, cut or tag a version, or what the next version should be.
---

# release

`docs/setup.md` § 7 is the procedure and the authority; this skill drives it and owns the parts that need judgment. If
the two disagree, the doc wins and the skill is fixed in the same change.

What a release is here:

- **One version names the binary and the extension.** `Cargo.toml` and `extension/manifest.json` carry it, the tag is
  `vX.Y.Z`, and `just check-tag vX.Y.Z` refuses a tag that does not match both. The extension's version rises even when
  its code did not change, which is harmless: Chrome installs the package within about 10 minutes, with no restart.
- **The artifacts are the tag and a GitHub release** carrying `extension/tab-keeper.crx` and notes. There is no
  changelog file; the notes live in the release.
- **CI checks, people publish.** On a `v*` tag CI runs every gate plus `release_check` (`just check-tag`) and
  `package_extension` (the committed package is what the signing key makes of this source). The GitHub release is
  created by hand, after those are green.
- **Consumers who follow `main` already run the code.** The release names it, tests it on branded Chrome, and gives
  everyone else a signed package and an account of what changed.

Three stops need the person's explicit yes: the **version** (Phase 3), the **notes** (Phase 5) and the **push of the
tag** (Phase 6). A pushed tag is public and is not moved afterwards.

## Phase 1 — Baseline

```bash
git switch main && git pull --ff-only && git status --short   # clean, up to date
git tag -l 'v*' --sort=-v:refname | head -3                   # the last release
gh release list --limit 3                                     # tags and releases should agree
gh run list --branch main --limit 3                           # main is green before anything is cut
```

## Phase 2 — Evidence

```bash
git log --oneline LAST..main --no-merges
```

Commit types are pointers, not verdicts: a `refactor` can change what an agent sees, and a `fix` can change a contract.
Diff each surface:

| Surface                                       | Evidence                                                                                                                                         |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| CLI: commands, flags, exit statuses           | `git diff LAST..main -- src/main.rs`; the Commands and Exit status tables in `docs/functionality.md`                                             |
| MCP tools: names, arguments, annotations      | `git diff LAST..main -- tests/fixtures/tools-list.json`, the reviewed `tools/list` snapshot. Up to v0.1.1, which predates it, `src/mcp/tools.rs` |
| MCP behaviour: what a call does               | `src/mcp.rs`, `src/mcp/`; the MCP tools section of `docs/functionality.md`                                                                       |
| Extension: permissions, operations            | `git diff LAST..main -- extension/manifest.json extension/*.js`. A new permission is a security change and is named in the notes                 |
| Socket protocol                               | The socket protocol section of `docs/functionality.md`; `src/host.rs`, `src/host/`                                                               |
| Saved records                                 | `src/saved.rs`. Records written by the last release must still restore                                                                           |
| Nix: packages, NixOS and Home Manager options | `git diff LAST..main -- modules/ flake.nix`. A renamed option breaks every configuration that sets it                                            |
| The guarantees                                | `git diff LAST..main -- docs/functionality.md`. A guarantee is never weakened; if one moved, stop and say so                                     |

## Phase 3 — Verdict

Classify every surface as **unchanged**, **additive** or **breaking**, each with the diff or commit behind it, then
propose the version. While the version is 0.x (`docs/functionality.md`: a release may rename commands and flags):

- anything breaking, a new tool or command, or a tool that now behaves differently → **minor** (`0.Y.0`)
- fixes and internal changes only → **patch** (`0.Y.Z`)
- nothing user-facing → say so, and ask whether a release is wanted at all

Versions continue from the tags, never from what `Cargo.toml` happens to say.

**Stop. The person confirms the version before anything changes.**

## Phase 4 — Prepare

File a bead for the release (`Release tab-keeper X.Y.Z`) and work on a branch named after it
(`chore/<id>-release-X.Y.Z`):

1. Set `version` in `Cargo.toml` and `extension/manifest.json`, then `cargo update -p tab-keeper`.
1. Re-sign: `nix develop .#e2e --command just pack`. It needs an age identity the signing key is encrypted to, and
   writes `extension/tab-keeper.crx` and `tests/vm/tab-keeper-next.crx`. Never replace
   `extension/signing-key.sops.json`: a new key is a new extension ID.
1. `nix develop --command just check-tag vX.Y.Z`, the same check CI runs on the tag.
1. Gates, each judged by its exit status (`cmd >log 2>&1; rc=$?`), never by a pipeline's last stage:

   ```bash
   nix develop --command just check
   nix flake check
   nix develop .#e2e --command just e2e
   just vm-test
   just vm-test-chrome
   just vm-test-chrome-upgrade
   ```

   The three VM tests need KVM on x86_64 and take minutes each; CI cannot run them, so a release is where they run.
   Record each one's result on the release's bead.

1. Commit the version files, `Cargo.lock` and both packages together as `chore(<id>): release tab-keeper X.Y.Z`. One
   commit goes onto `main` by fast-forward; a branch that also carries other work is merged with `--no-ff`. The tag goes
   on the commit `main` then points at, after the person agrees to the change reaching `main`.

## Phase 5 — Notes

Write the notes to a file outside the repository, in the shape of the earlier releases (`gh release view LAST`):

- a short opening paragraph saying what the release is about;
- what changed, by what a person or an agent will notice, with the why. Breaking changes first, each saying what the
  reader must do;
- **Upgrading**: what an installed machine must do, if anything (Chrome picks the extension up by itself);
- **Tested on**: which Chromium and which branded Chrome the suites and VM tests ran against;
- **Known limits**: carried over from the last release, minus what this one fixed, plus anything new.

The repository is public and so are the notes: no personal data, no paths or names from the machine the release was cut
on. Examples use invented data like `example.com`.

**Stop. The person approves the notes.**

## Phase 6 — Publish

```bash
git tag -a vX.Y.Z -m "tab-keeper X.Y.Z"
```

**Stop. The person confirms the push**, then:

```bash
git push origin main && git push origin vX.Y.Z
gh run list --branch vX.Y.Z --limit 1    # then gh run watch <id>; release_check and package_extension must pass
```

Only once the tag's run is green:

```bash
gh release create vX.Y.Z extension/tab-keeper.crx --title "tab-keeper X.Y.Z" --notes-file <notes>
gh release view vX.Y.Z
```

Close the release's bead with the release URL, and report the version, the URL and which gates ran where.

## Gotchas

- **Commit `Cargo.lock` with the version.** `check-tag` reads the binary through `cargo run`, which quietly rewrites the
  lock on disk, so the check passes locally while the Nix build reads the committed lock.
- **The upgrade test's "released" machine starts from 0.1.0**, kept as `tests/vm/tab-keeper-0.1.0.crx`, not from the
  last release. That still proves the oldest install upgrades, and its other two machines prove this release updates to
  a later one. If the extension's update code changed since the last release, also test from the last release's own
  package, which is attached to its GitHub release.
- **A red `package_extension` on the tag** means the committed `.crx` is not what the key makes of the tagged source: an
  extension file changed after `just pack`. Re-pack, commit, and tag again before anything is published; a tag nobody
  has published from can still be deleted and recreated.
- **Notes are not generated from commits.** The commit log is the evidence; the notes are written for the person
  installing it.
