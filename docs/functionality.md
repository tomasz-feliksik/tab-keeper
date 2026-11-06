# Functionality

What tab-keeper does and promises. **All of this is the design and nothing is built yet.** The prototype in
`reference/prototype/` does `list` and `discard` only. Names and flags may change while a milestone is being built; the
guarantees should not.

## Addressing a tab

Chrome's tab IDs are only meaningful inside one profile and one browser session, so a bare ID is not enough. Every
operation that changes a tab takes the profile, the tab ID and the URL the caller believes is there. If the URL no
longer matches, that tab is refused and reported. This is how a plan made from a list that is now stale fails safely.

## Commands

| Command                                       | What it does                                                | Milestone |
| --------------------------------------------- | ----------------------------------------------------------- | --------- |
| `tab-keeper list [--profile P] [--json]`      | Every tab in every running profile, with paging and filters | M1        |
| `tab-keeper discard --idle 2h [--dry-run]`    | Unload idle tabs to free memory; they stay in the tab strip | M1        |
| `tab-keeper close <tabs>`                     | Save the tabs, then close them                              | M2        |
| `tab-keeper restore <record>`                 | Reopen a saved set in the same profile                      | M2        |
| `tab-keeper move <tabs> --window W --index I` | Rearrange tabs inside a profile                             | M2        |
| `tab-keeper group <tabs> --title T --color C` | Create or extend a tab group                                | M2        |
| `tab-keeper ungroup <tabs>`                   | Take tabs out of a group, leaving them open                 | M2        |
| `tab-keeper mcp`                              | Run the MCP server over stdio                               | M3        |
| `tab-keeper bookmark add\|list\|remove`       | Manage bookmarks in a profile                               | M4        |
| `tab-keeper group save\|open\|list\|delete`   | Close a group to a named record and open it later           | M5        |
| `tab-keeper host`                             | The native messaging relay. Chrome runs this, not people    | M1        |

## MCP tools

Read tools and write tools are separate, so a harness can allow reading freely and ask about writing. Annotations have
to be true, because they are what the permission prompt relies on.

| Tool                                        | Annotation          | Milestone |
| ------------------------------------------- | ------------------- | --------- |
| `list_profiles`, `list_tabs`, `list_groups` | read-only           | M3        |
| `list_saved`                                | read-only           | M3        |
| `discard_tabs`                              | write               | M3        |
| `move_tabs`, `group_tabs`, `ungroup_tabs`   | write               | M3        |
| `restore_saved`                             | write               | M3        |
| `close_tabs`                                | destructive         | M3        |
| `list_bookmarks`                            | read-only           | M4        |
| `add_bookmark`, `remove_bookmark`           | write / destructive | M4        |
| `list_saved_groups`                         | read-only           | M5        |
| `save_group`, `open_saved_group`            | destructive / write | M5        |

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

## Files

| What              | Where                                       |
| ----------------- | ------------------------------------------- |
| Sockets           | `$XDG_RUNTIME_DIR/tab-keeper/`, mode 0700   |
| Saved tabs        | `$XDG_STATE_HOME/tab-keeper/`, mode 0700    |
| Host manifest     | Chrome's per-user `native-messaging-hosts/` |
| Extension install | Machine-wide Chrome policy                  |

Saved records contain URLs and titles, which can be private. They stay on this machine and are readable only by this
user.
