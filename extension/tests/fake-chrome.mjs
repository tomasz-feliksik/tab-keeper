// A small, faithful stand-in for the Chrome APIs, including the behaviours measured on
// Chromium 156 that make these limits necessary: discard works on an active tab,
// grouping unpins a pinned tab, and a pinned tab keeps its place in front.

export function fakeChrome(initialTabs = []) {
  const tabs = new Map(initialTabs.map((tab, index) => [tab.id, withDefaults(tab, index)]));
  const groups = new Map();
  const windows = new Set([...tabs.values()].map((tab) => tab.windowId));
  const storage = {};
  const calls = [];
  const bookmarks = new Map([["0", { id: "0", title: "", children: [] }]]);
  let nextId = 1000;

  const need = (id) => {
    const tab = tabs.get(id);
    if (!tab) throw new Error(`No tab with id: ${id}.`);
    return tab;
  };
  const subtree = (id) => {
    const node = bookmarks.get(id);
    if (!node) throw new Error("Can't find bookmark for id.");
    return node;
  };
  const dropNode = (id) => {
    const node = subtree(id);
    for (const child of node.children ?? []) dropNode(child.id);
    bookmarks.delete(id);
    const parent = bookmarks.get(node.parentId);
    if (parent) parent.children = parent.children.filter((child) => child.id !== id);
  };

  return {
    calls,
    state: { tabs, groups, windows, storage, bookmarks },
    runtime: { id: "fakeextensionid" },
    storage: {
      local: {
        get: async (key) => (key in storage ? { [key]: storage[key] } : {}),
        set: async (values) => Object.assign(storage, values),
      },
    },
    tabs: {
      query: async () => [...tabs.values()].map((tab) => ({ ...tab })),
      get: async (id) => ({ ...need(id) }),
      discard: async (id) => {
        calls.push(["discard", id]);
        const tab = need(id);
        tab.discarded = true;
        return { ...tab };
      },
      remove: async (id) => {
        calls.push(["remove", id]);
        need(id);
        tabs.delete(id);
      },
      move: async (id, { index, windowId }) => {
        calls.push(["move", id, index]);
        const tab = need(id);
        if (windowId !== undefined) {
          if (!windows.has(windowId)) throw new Error(`No window with id: ${windowId}.`);
          tab.windowId = windowId;
        }
        const pinnedCount = [...tabs.values()].filter(
          (other) => other.windowId === tab.windowId && other.pinned && other !== tab,
        ).length;
        tab.index = !tab.pinned && index !== -1 && index < pinnedCount ? pinnedCount : index;
        return { ...tab };
      },
      group: async ({ tabIds, groupId, createProperties }) => {
        calls.push(["group", tabIds]);
        const id = groupId ?? nextId++;
        if (!groups.has(id)) {
          const windowId = createProperties?.windowId ?? need(tabIds[0]).windowId;
          groups.set(id, { id, title: "", color: "grey", collapsed: false, windowId });
        }
        for (const tabId of tabIds) {
          const tab = need(tabId);
          tab.groupId = id;
          tab.pinned = false;
          tab.windowId = groups.get(id).windowId;
        }
        return id;
      },
      ungroup: async (id) => {
        calls.push(["ungroup", id]);
        need(id).groupId = -1;
      },
      create: async ({ windowId, url, index, pinned }) => {
        calls.push(["create", url]);
        const tab = withDefaults({ id: nextId++, windowId, url, pinned, index }, tabs.size);
        tabs.set(tab.id, tab);
        return { ...tab };
      },
      update: async (id, properties) => Object.assign(need(id), properties),
    },
    tabGroups: {
      query: async () => [...groups.values()].map((group) => ({ ...group })),
      update: async (id, properties) => Object.assign(groups.get(id), properties),
    },
    windows: {
      get: async (id) => {
        if (!windows.has(id)) throw new Error(`No window with id: ${id}.`);
        return { id };
      },
      create: async ({ url }) => {
        const id = nextId++;
        windows.add(id);
        const tab = withDefaults({ id: nextId++, windowId: id, url, index: 0 }, 0);
        tabs.set(tab.id, tab);
        return { id, tabs: [{ ...tab }] };
      },
    },
    bookmarks: {
      getTree: async () => [subtree("0")],
      getSubTree: async (id) => [subtree(id)],
      create: async ({ title, url, parentId = "0", index }) => {
        calls.push(["bookmark", title]);
        const id = String(nextId++);
        const node = { id, title, parentId, ...(url === undefined ? { children: [] } : { url }) };
        bookmarks.set(id, node);
        const siblings = subtree(parentId).children;
        siblings.splice(index ?? siblings.length, 0, node);
        return node;
      },
      remove: async (id) => dropNode(id),
      removeTree: async (id) => dropNode(id),
    },
  };
}

function withDefaults(tab, index) {
  return {
    windowId: 1,
    index,
    title: `title ${tab.id}`,
    url: `https://example.com/${tab.id}`,
    active: false,
    pinned: false,
    audible: false,
    discarded: false,
    groupId: -1,
    lastAccessed: 1000,
    ...tab,
    pinned: Boolean(tab.pinned),
  };
}
