# Functionality

What tab-keeper does and promises. Everything here is built and tested against Chromium, and every kind of command has
run on branded Chrome in a NixOS VM. It is installed on one real machine, and has not yet seen days of use. Names and
flags may change before a first release; the guarantees should not.

## Addressing a tab

Chrome's tab IDs are only meaningful inside one profile and one browser session, so a bare ID is not enough. Every
operation that changes a tab takes the profile, the tab ID and the URL the caller believes is there. If the URL no
longer matches, that tab is refused and reported. This is how a plan made from a list that is now stale fails safely.

Discarding can give a tab a new ID, as branded Chrome does. `discard` therefore answers each unloaded tab with a
`new_ref`, and a person sees `old is now new`. The old reference names nothing from then on and fails as `gone`.

## Commands

| Command                                                 | What it does                                                |
| ------------------------------------------------------- | ----------------------------------------------------------- |
| `tab-keeper list [--profile P] [--host H] [--search S]` | Every tab in every running profile, with paging and filters |
| `tab-keeper profiles`                                   | Connected profiles, and those known but not connected       |
| `tab-keeper discard --idle 2h [--dry-run]`              | Unload idle tabs to free memory; they stay in the tab strip |
| `tab-keeper discard <tabs>`                             | Unload these tabs                                           |
| `tab-keeper close <tabs> [--allow pinned]`              | Save the tabs, then close them                              |
| `tab-keeper restore <record> [--again]`                 | Reopen a saved set in the same profile                      |
| `tab-keeper saved`                                      | What has been saved                                         |
| `tab-keeper move <tabs> [--window W] --index I`         | Rearrange tabs inside a profile                             |
| `tab-keeper group <tabs> [--title T] [--color C]`       | Create or extend a tab group                                |
| `tab-keeper ungroup <tabs>`                             | Take tabs out of a group, leaving them open                 |
| `tab-keeper bookmark add\|list\|remove\|restore`        | Manage bookmarks in a profile                               |
| `tab-keeper group save\|open\|list\|delete`             | Close a group to a named record and open it later           |
| `tab-keeper mcp`                                        | Run the MCP server over stdio                               |
| `tab-keeper host`                                       | The native messaging relay. Chrome runs this, not people    |

`--json` prints what a program needs, and `--timeout` sets how long to wait for each profile. Every `<tabs>` is a
reference printed by `list`, of the form `profile:id:urlhash`.

| Exit status | Meaning                                                                         |
| ----------- | ------------------------------------------------------------------------------- |
| 0           | Everything asked for happened                                                   |
| 2           | Part of it did not: a profile that did not answer, or a tab refused or failed   |
| 1           | Nothing was attempted, such as no profile connected or a policy that is missing |

`discard` needs `TabDiscardingExceptions` set in Chrome's policy, even to an empty list, and refuses to run without it.
That setting belongs to the machine's own configuration, not to this project.

## MCP tools

Read tools and write tools are separate, so a harness can allow reading freely and ask about writing. Annotations have
to be true, because they are what the permission prompt relies on.

| Tool                                        | Annotation          |
| ------------------------------------------- | ------------------- |
| `list_profiles`, `list_tabs`, `list_groups` | read-only           |
| `list_saved`                                | read-only           |
| `discard_tabs`                              | write               |
| `move_tabs`, `group_tabs`, `ungroup_tabs`   | write               |
| `restore_saved`                             | write               |
| `close_tabs`                                | destructive         |
| `list_bookmarks`                            | read-only           |
| `add_bookmark`, `remove_bookmark`           | write / destructive |
| `list_saved_groups`                         | read-only           |
| `save_group`, `open_saved_group`            | destructive / write |

Every result carries a `data_label` saying its titles and URLs are data, and those fields are shortened and stripped of
invisible characters. A result that is only partly true has `isError` set while keeping what it has, so a failure can
never read as an empty answer.

The expected way of working:

1. The agent reads a compact list, one page at a time.
1. It presents a plan: what it would close, how it would group the rest, and why.
1. The person changes the plan or approves it.
1. The agent runs the operations. The harness asks for permission on destructive ones.
1. The person can restore anything closed.

## Guarantees

These are the product. Each one needs a test that fails when the guarantee is broken.

1. **Nothing is closed before it is saved.** The record is written and flushed to disk first. If it cannot be written,
   nothing closes.
1. **A restore puts back what it can and says what it could not.** The URL, title, profile, window, position, pinned
   state and group go into the record. Page state, scroll position and form contents cannot be saved.
1. **Tabs in use are protected.** Active, pinned and audible tabs are skipped unless the caller names that protection in
   the request. `discard` also honours the browser's `TabDiscardingExceptions` policy, and refuses to run if it cannot
   read that policy.
1. **A stale plan fails safely.** See [Addressing a tab](#addressing-a-tab).
1. **"Could not check" is never success.** A profile that does not answer is reported, the result is marked partial and
   the exit status is non-zero. It never looks like a profile with no tabs.
1. **Every item in a batch has its own result.** One failing tab does not hide the others, and the caller learns exactly
   which tabs changed.
1. **No network listener.** Only native messaging and Unix sockets that only this user can open.
1. **The extension cannot read pages.** Its permissions are limited to what the current milestone needs, with no host
   permissions, `scripting`, `debugger` or `cookies`. A test reads the manifest and fails on any addition.
1. **The extension enforces the limits.** Protections are checked inside the extension as well, so a faulty or
   impersonated client cannot skip them. Each capability can be switched off there.
1. **Output is bounded.** Lists are paged and compact by default, so a large browser cannot flood an agent's context.

## The socket protocol

What a client of `tab-keeper host` can rely on. Messages use native messaging's framing, a native-endian `u32` length
and then JSON, on the socket as well as on Chrome's pipes.

1. Connect to `$XDG_RUNTIME_DIR/tab-keeper/<instance>.sock` and send one JSON object of up to 1 MiB, including the `id`
   the host adds. The host replaces any `id` the client sent.
1. Read one frame. It is the extension's answer exactly as sent, or `{"ok": false, "error": "…"}` from the host.
1. The connection then closes, so each request needs a new connection.

The connection ending without a complete frame means there is no answer. A client has to report that as a failure, along
with a refused connection, which means a socket left by a host that was killed.

## Files

| What              | Where                                     |
| ----------------- | ----------------------------------------- |
| Sockets           | `$XDG_RUNTIME_DIR/tab-keeper/`, mode 0700 |
| Saved tabs        | `$XDG_STATE_HOME/tab-keeper/`, mode 0700  |
| Host manifest     | Chrome's per-user `NativeMessagingHosts/` |
| Extension install | Machine-wide Chrome policy                |

Saved records contain URLs and titles, which can be private. They stay on this machine and are readable only by this
user.
