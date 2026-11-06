import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, test } from "node:test";

import { CAPABILITIES, createAnswerer } from "../core.js";
import { fakeChrome } from "./fake-chrome.mjs";

const target = (tab) => ({ id: tab.id, url: tab.url ?? `https://example.com/${tab.id}` });

async function ask(chrome, request) {
  const response = await createAnswerer(chrome)({ id: 7, ...request });
  assert.equal(response.id, 7, "every answer carries the request's id");
  return response;
}

async function results(chrome, request) {
  const response = await ask(chrome, request);
  assert.equal(response.ok, true, response.error);
  return response.result.results;
}

describe("the manifest", () => {
  const manifest = JSON.parse(readFileSync(new URL("../manifest.json", import.meta.url)));

  test("asks for exactly the agreed permissions", () => {
    assert.deepEqual([...manifest.permissions].sort(), [
      "alarms", // reconnects after the host dies or the worker is evicted
      "bookmarks", // tab-keeper bookmark
      "nativeMessaging", // the only way clients reach this extension
      "storage", // the instance id and the capability switches
      "tabGroups", // reading and naming groups
      "tabs", // titles and URLs
    ]);
  });

  test("has no way to read pages or be reached from one", () => {
    for (const forbidden of [
      "host_permissions",
      "optional_permissions",
      "optional_host_permissions",
      "content_scripts",
      "externally_connectable",
      "web_accessible_resources",
    ]) {
      assert.equal(manifest[forbidden], undefined, forbidden);
    }
  });
});

describe("answering", () => {
  test("an unknown operation is an error, including inherited property names", async () => {
    for (const op of ["nonsense", "constructor", "__proto__", undefined]) {
      const response = await ask(fakeChrome(), { op });
      assert.equal(response.ok, false, String(op));
      assert.match(response.error, /unknown operation/);
    }
  });

  test("a failure in Chrome comes back as an error rather than being lost", async () => {
    const chrome = fakeChrome();
    chrome.tabs.query = async () => {
      throw new Error("boom");
    };
    const response = await ask(chrome, { op: "list" });
    assert.deepEqual(response, { id: 7, ok: false, error: "boom" });
  });
});

describe("capability switches", () => {
  const requests = {
    list: { op: "list" },
    discard: { op: "discard", targets: [] },
    close: { op: "close", targets: [] },
    restore: { op: "restore", tabs: [] },
    move: { op: "move", targets: [], index: 0 },
    group: { op: "group", targets: [] },
    bookmarks: { op: "bookmarks_list" },
  };

  for (const capability of CAPABILITIES) {
    test(`switching off ${capability} refuses it and nothing else`, async () => {
      const chrome = fakeChrome();
      chrome.state.storage.capabilities = { [capability]: false };

      const response = await ask(chrome, requests[capability]);

      assert.equal(response.ok, false);
      assert.match(response.error, new RegExp(`${capability} capability is switched off`));
      for (const [other, request] of Object.entries(requests)) {
        if (other !== capability) assert.equal((await ask(chrome, request)).ok, true, other);
      }
    });
  }

  test("ungroup and the bookmark writes obey their capability", async () => {
    const chrome = fakeChrome();
    chrome.state.storage.capabilities = { group: false, bookmarks: false };
    for (const request of [
      { op: "ungroup", targets: [] },
      { op: "bookmarks_add", items: [] },
      { op: "bookmarks_remove", nodeId: "1" },
      { op: "bookmarks_restore", node: {} },
    ]) {
      assert.match((await ask(chrome, request)).error, /switched off/, request.op);
    }
  });
});

describe("list", () => {
  test("gives titles, URLs, state and groups, and nothing it was not asked for", async () => {
    const chrome = fakeChrome([{ id: 1, title: "A", url: "https://a/", secret: "x" }]);
    chrome.state.groups.set(5, { id: 5, title: "G", color: "red", collapsed: false, windowId: 1 });

    const { result } = await ask(chrome, { op: "list" });

    assert.equal(result.extensionId, "fakeextensionid");
    assert.equal(result.tabs[0].title, "A");
    assert.equal(result.tabs[0].secret, undefined);
    assert.deepEqual(result.groups, [{ id: 5, title: "G", color: "red", collapsed: false, windowId: 1 }]);
  });
});

const destructive = [
  ["discard", "discard"],
  ["close", "remove"],
];

for (const [op, call] of destructive) {
  describe(op, () => {
    test("acts on an ordinary tab", async () => {
      const chrome = fakeChrome([{ id: 1 }]);
      const [result] = await results(chrome, { op, targets: [target({ id: 1 })] });
      assert.equal(result.status, "done");
      assert.deepEqual(chrome.calls, [[call, 1]]);
    });

    for (const protection of ["active", "pinned", "audible"]) {
      test(`skips a ${protection} tab unless the caller names that protection`, async () => {
        const chrome = fakeChrome([{ id: 1, [protection]: true }]);
        const targets = [target({ id: 1 })];

        const [skipped] = await results(chrome, { op, targets });
        assert.deepEqual(skipped, { id: 1, status: "refused", reason: protection });
        assert.deepEqual(chrome.calls, [], "Chrome was not asked");

        const [other] = await results(chrome, {
          op,
          targets,
          allow: PROTECTION_OTHER_THAN[protection],
        });
        assert.equal(other.status, "refused", "naming another protection does not help");

        const [allowed] = await results(chrome, { op, targets, allow: [protection] });
        assert.equal(allowed.status, "done");
      });
    }

    test("refuses a tab whose URL is not the one the caller believes is there", async () => {
      const chrome = fakeChrome([{ id: 1, url: "https://now/" }]);
      const [result] = await results(chrome, {
        op,
        targets: [{ id: 1, url: "https://then/" }],
      });
      assert.deepEqual(result, { id: 1, status: "refused", reason: "url_changed" });
      assert.deepEqual(chrome.calls, []);
    });

    test("a missing tab and a malformed target are refused, one by one", async () => {
      const chrome = fakeChrome([{ id: 1 }]);
      const outcome = await results(chrome, {
        op,
        targets: [{ id: 9, url: "x" }, { id: "1", url: "x" }, { id: 1 }, null, target({ id: 1 })],
      });
      assert.deepEqual(
        outcome.map((result) => result.reason ?? result.status),
        ["gone", "bad_target", "bad_target", "bad_target", "done"],
      );
    });

    test("one failure does not hide the other tabs' results", async () => {
      const chrome = fakeChrome([{ id: 1 }, { id: 2 }, { id: 3 }]);
      const real = chrome.tabs[call];
      chrome.tabs[call] = async (id) => {
        if (id === 2) throw new Error("no");
        return real(id);
      };
      const outcome = await results(chrome, {
        op,
        targets: [target({ id: 1 }), target({ id: 2 }), target({ id: 3 })],
      });
      assert.deepEqual(
        outcome.map((result) => result.status),
        ["done", "failed", "done"],
      );
      assert.equal(outcome[1].reason, "no");
    });

    test("allow may not name anything but a protection", async () => {
      const response = await ask(fakeChrome([{ id: 1 }]), {
        op,
        targets: [target({ id: 1 })],
        allow: ["everything"],
      });
      assert.equal(response.ok, false);
    });

    test("targets must be a list", async () => {
      assert.equal((await ask(fakeChrome(), { op })).ok, false);
    });
  });
}

const PROTECTION_OTHER_THAN = { active: ["pinned"], pinned: ["audible"], audible: ["active"] };

describe("discard", () => {
  test("an already discarded tab gets its own failure", async () => {
    const chrome = fakeChrome([{ id: 1, discarded: true }]);
    const [result] = await results(chrome, { op: "discard", targets: [target({ id: 1 })] });
    assert.equal(result.status, "failed");
    assert.deepEqual(chrome.calls, []);
  });
});

describe("move", () => {
  test("reports where a tab really ended up, not where it was asked to go", async () => {
    const chrome = fakeChrome([
      { id: 1, pinned: true, index: 0 },
      { id: 2, index: 1 },
    ]);

    const [result] = await results(chrome, {
      op: "move",
      targets: [target({ id: 2 })],
      index: 0,
    });

    assert.equal(result.requestedIndex, 0);
    assert.equal(result.index, 1, "a pinned tab stays in front");
    assert.equal(result.windowId, 1);
  });

  test("consecutive tabs go to consecutive positions", async () => {
    const chrome = fakeChrome([{ id: 1 }, { id: 2 }]);
    await results(chrome, {
      op: "move",
      targets: [target({ id: 1 }), target({ id: 2 })],
      index: 4,
    });
    assert.deepEqual(chrome.calls, [
      ["move", 1, 4],
      ["move", 2, 5],
    ]);
  });

  test("a refused tab takes no position", async () => {
    const chrome = fakeChrome([{ id: 1 }, { id: 2 }]);
    await results(chrome, {
      op: "move",
      targets: [{ id: 1, url: "stale" }, target({ id: 2 })],
      index: 4,
    });
    assert.deepEqual(chrome.calls, [["move", 2, 4]]);
  });

  test("a window that does not exist fails that tab, and -1 stays the end", async () => {
    const chrome = fakeChrome([{ id: 1 }, { id: 2 }]);
    const [first, second] = await results(chrome, {
      op: "move",
      targets: [target({ id: 1 }), target({ id: 2 })],
      index: -1,
      windowId: 99,
    });
    assert.equal(first.status, "failed");
    assert.equal(second.status, "failed");
    assert.deepEqual(chrome.calls, [
      ["move", 1, -1],
      ["move", 2, -1],
    ]);
  });

  test("a nonsense index is refused before anything moves", async () => {
    for (const index of [-2, 1.5, "0", undefined]) {
      const chrome = fakeChrome([{ id: 1 }]);
      const response = await ask(chrome, { op: "move", targets: [target({ id: 1 })], index });
      assert.equal(response.ok, false, String(index));
      assert.deepEqual(chrome.calls, []);
    }
  });
});

describe("group", () => {
  test("creates a named group and reports each tab's final place", async () => {
    const chrome = fakeChrome([{ id: 1 }, { id: 2, windowId: 2 }]);

    const { result } = await ask(chrome, {
      op: "group",
      targets: [target({ id: 1 }), target({ id: 2 })],
      title: "T",
      color: "cyan",
    });

    assert.equal(chrome.state.groups.get(result.groupId).title, "T");
    assert.equal(chrome.state.groups.get(result.groupId).color, "cyan");
    const [, second] = result.results;
    assert.equal(second.fromWindowId, 2);
    assert.equal(second.windowId, 1, "grouping pulled the tab into the first tab's window");
    assert.equal(second.groupId, result.groupId);
  });

  test("a pinned tab is protected, because grouping would silently unpin it", async () => {
    const chrome = fakeChrome([{ id: 1, pinned: true }, { id: 2 }]);

    const { result } = await ask(chrome, {
      op: "group",
      targets: [target({ id: 1 }), target({ id: 2 })],
    });

    assert.deepEqual(result.results[0], { id: 1, status: "refused", reason: "pinned" });
    assert.equal(chrome.state.tabs.get(1).pinned, true);
    assert.equal(result.results[1].status, "done");
  });

  test("naming the protection lets a pinned tab be grouped", async () => {
    const chrome = fakeChrome([{ id: 1, pinned: true }]);
    const { result } = await ask(chrome, {
      op: "group",
      targets: [target({ id: 1 })],
      allow: ["pinned"],
    });
    assert.equal(result.results[0].status, "done");
    assert.equal(result.results[0].pinned, false, "the result shows it lost its pin");
  });

  test("a stale tab is refused, and with nothing left no group is made", async () => {
    const chrome = fakeChrome([{ id: 1 }]);
    const { result } = await ask(chrome, { op: "group", targets: [{ id: 1, url: "stale" }] });
    assert.equal(result.groupId, null);
    assert.equal(result.results[0].reason, "url_changed");
    assert.equal(chrome.state.groups.size, 0);
  });

  test("extends an existing group when given one", async () => {
    const chrome = fakeChrome([{ id: 1 }, { id: 2 }]);
    const first = await ask(chrome, { op: "group", targets: [target({ id: 1 })], title: "T" });
    const second = await ask(chrome, {
      op: "group",
      targets: [target({ id: 2 })],
      groupId: first.result.groupId,
    });
    assert.equal(second.result.groupId, first.result.groupId);
    assert.equal(chrome.state.tabs.get(2).groupId, first.result.groupId);
  });

  test("a failure while grouping fails every accepted tab", async () => {
    const chrome = fakeChrome([{ id: 1 }, { id: 2 }]);
    chrome.tabs.group = async () => {
      throw new Error("no");
    };
    const { result } = await ask(chrome, {
      op: "group",
      targets: [target({ id: 1 }), { id: 2, url: "stale" }],
    });
    assert.deepEqual(
      result.results.map((entry) => entry.status),
      ["failed", "refused"],
    );
  });

  test("ungroup leaves the tab open and reports it out of the group", async () => {
    const chrome = fakeChrome([{ id: 1, groupId: 5 }]);
    const [result] = await results(chrome, { op: "ungroup", targets: [target({ id: 1 })] });
    assert.equal(result.groupId, -1);
    assert.equal(chrome.state.tabs.has(1), true);
  });
});

describe("restore", () => {
  test("reuses a window that is still open, keeping position and pin", async () => {
    const chrome = fakeChrome([{ id: 1, windowId: 1 }]);
    const [first, second] = await results(chrome, {
      op: "restore",
      tabs: [
        { url: "https://b/", window: 1, index: 3, pinned: false },
        { url: "https://a/", window: 1, index: 0, pinned: true },
      ],
    });
    assert.equal(first.position, 0);
    assert.equal(first.index, 3);
    assert.equal(second.index, 0);
    assert.equal(second.pinned, true);
    assert.deepEqual(chrome.calls, [
      ["create", "https://a/"],
      ["create", "https://b/"],
    ]);
  });

  test("a window that no longer exists is created once for all its tabs", async () => {
    const chrome = fakeChrome();
    const outcome = await results(chrome, {
      op: "restore",
      tabs: [
        { url: "https://a/", window: 42, index: 0, pinned: true },
        { url: "https://b/", window: 42, index: 1 },
      ],
    });
    assert.equal(chrome.state.windows.size, 1);
    assert.equal(outcome[0].windowId, outcome[1].windowId);
    assert.equal(outcome[0].pinned, true);
  });

  test("groups are recreated with their title, colour and tabs", async () => {
    const chrome = fakeChrome();
    const outcome = await results(chrome, {
      op: "restore",
      tabs: [
        { url: "https://a/", window: 42, index: 0, group: "g1" },
        { url: "https://b/", window: 42, index: 1, group: "g1" },
      ],
      groups: [{ key: "g1", title: "T", color: "cyan", collapsed: true }],
    });
    assert.equal(outcome[0].groupId, outcome[1].groupId);
    const group = chrome.state.groups.get(outcome[0].groupId);
    assert.deepEqual(
      { title: group.title, color: group.color, collapsed: group.collapsed },
      { title: "T", color: "cyan", collapsed: true },
    );
  });

  test("a group that cannot be recreated is reported, and its tabs stay open", async () => {
    const chrome = fakeChrome();
    chrome.tabs.group = async () => {
      throw new Error("no");
    };
    const { result } = await ask(chrome, {
      op: "restore",
      tabs: [{ url: "https://a/", window: 42, index: 0, group: "g1" }],
      groups: [{ key: "g1", title: "T" }],
    });
    assert.deepEqual(result.groupProblems, [{ key: "g1", reason: "no" }]);
    assert.equal(result.results[0].status, "done");
  });

  test("a javascript: URL is refused and the rest still open", async () => {
    const chrome = fakeChrome();
    const outcome = await results(chrome, {
      op: "restore",
      tabs: [
        { url: " JavaScript:alert(1)", window: 1, index: 0 },
        { url: "", window: 1, index: 1 },
        { url: "https://a/", window: 42, index: 2 },
      ],
    });
    assert.deepEqual(
      outcome.map((result) => result.status),
      ["refused", "refused", "done"],
    );
    assert.equal(outcome[0].reason, "not_openable");
  });

  test("a tab Chrome will not open gets its own failure", async () => {
    const chrome = fakeChrome([{ id: 1, windowId: 1 }]);
    chrome.tabs.create = async () => {
      throw new Error("no");
    };
    const [result] = await results(chrome, {
      op: "restore",
      tabs: [{ url: "https://a/", window: 1, index: 0 }],
    });
    assert.equal(result.status, "failed");
    assert.equal(result.reason, "no");
  });
});

describe("bookmarks", () => {
  test("adds a folder of items and lists them back", async () => {
    const chrome = fakeChrome();
    const { result } = await ask(chrome, {
      op: "bookmarks_add",
      folder: "F",
      items: [{ title: "A", url: "https://a/" }],
    });
    assert.equal(result.results[0].status, "done");

    const listed = await ask(chrome, { op: "bookmarks_list", nodeId: result.folderId });
    assert.equal(listed.result.tree[0].title, "F");
    assert.equal(listed.result.tree[0].children[0].url, "https://a/");
  });

  test("a javascript: bookmark is refused, because a click would run it", async () => {
    const chrome = fakeChrome();
    const { result } = await ask(chrome, {
      op: "bookmarks_add",
      items: [{ title: "x", url: "javascript:alert(1)" }],
    });
    assert.equal(result.results[0].reason, "not_openable");
    assert.deepEqual(chrome.calls, []);
  });

  test("a removal fails when the bookmark changed since it was read", async () => {
    const chrome = fakeChrome();
    const added = await ask(chrome, {
      op: "bookmarks_add",
      items: [{ title: "A", url: "https://a/" }],
    });
    const nodeId = added.result.results[0].bookmarkId;

    const stale = await ask(chrome, { op: "bookmarks_remove", nodeId, expectedTitle: "old" });
    assert.equal(stale.ok, false);
    assert.equal(chrome.state.bookmarks.has(nodeId), true);

    const removed = await ask(chrome, { op: "bookmarks_remove", nodeId, expectedTitle: "A" });
    assert.equal(removed.ok, true);
    assert.equal(chrome.state.bookmarks.has(nodeId), false);
  });

  test("a removed folder restores with its children", async () => {
    const chrome = fakeChrome();
    const added = await ask(chrome, {
      op: "bookmarks_add",
      folder: "F",
      items: [{ title: "A", url: "https://a/" }],
    });
    const nodeId = added.result.folderId;
    const [saved] = (await ask(chrome, { op: "bookmarks_list", nodeId })).result.tree;
    const snapshot = structuredClone(saved);
    await ask(chrome, { op: "bookmarks_remove", nodeId });
    assert.equal(chrome.state.bookmarks.has(nodeId), false);

    const { result } = await ask(chrome, { op: "bookmarks_restore", node: snapshot });

    const [restored] = (await ask(chrome, { op: "bookmarks_list", nodeId: result.bookmarkId })).result.tree;
    assert.equal(restored.title, "F");
    assert.equal(restored.children[0].url, "https://a/");
  });
});

test("a tab still loading is listed and addressed by the URL it is loading", async () => {
  const chrome = fakeChrome([{ id: 1, url: "", pendingUrl: "https://example.com/" }]);
  const answer = createAnswerer(chrome);

  const listed = await answer({ id: 1, op: "list" });
  assert.equal(listed.result.tabs[0].url, "https://example.com/");

  const closed = await answer({ id: 2, op: "close", targets: [{ id: 1, url: "https://example.com/" }] });
  assert.equal(closed.result.results[0].status, "done");
});

test("removing a bookmark folder removes what is in it, as the real API needs asking", async () => {
  const chrome = fakeChrome();
  const folder = await chrome.bookmarks.create({ title: "folder" });
  await chrome.bookmarks.create({ title: "inside", url: "https://example.com/", parentId: folder.id });

  const removed = await createAnswerer(chrome)({ id: 1, op: "bookmarks_remove", nodeId: folder.id });

  assert.equal(removed.ok, true, removed.error);
  assert.deepEqual(removed.result, { removed: folder.id });
});
