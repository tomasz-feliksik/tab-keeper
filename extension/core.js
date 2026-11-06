// What the extension will do when asked, and the limits it enforces. A client cannot
// skip these, so a faulty or impersonated client is still held to them. `chrome` is a
// parameter so all of this can be tested without a browser.

export const CAPABILITIES = ["list", "discard", "close", "restore", "move", "group", "bookmarks"];

const PROTECTIONS = ["active", "pinned", "audible"];
const NO_GROUP = -1;

/** Which capability each operation needs. An operation not listed here does not exist. */
const OPERATIONS = {
  list: { capability: "list", run: listTabs },
  discard: { capability: "discard", run: discardTabs },
  close: { capability: "close", run: closeTabs },
  restore: { capability: "restore", run: restoreTabs },
  move: { capability: "move", run: moveTabs },
  group: { capability: "group", run: groupTabs },
  ungroup: { capability: "group", run: ungroupTabs },
  bookmarks_list: { capability: "bookmarks", run: listBookmarks },
  bookmarks_add: { capability: "bookmarks", run: addBookmarks },
  bookmarks_remove: { capability: "bookmarks", run: removeBookmark },
  bookmarks_restore: { capability: "bookmarks", run: restoreBookmarks },
};

export function createAnswerer(chrome) {
  return async function answer(request) {
    const id = request?.id;
    try {
      const operation = Object.hasOwn(OPERATIONS, request?.op) ? OPERATIONS[request.op] : null;
      if (operation === null) throw new Error(`unknown operation: ${request?.op}`);
      if (!(await capabilityEnabled(chrome, operation.capability))) {
        throw new Error(`the ${operation.capability} capability is switched off in this profile`);
      }
      return { id, ok: true, result: await operation.run(chrome, request) };
    } catch (error) {
      return { id, ok: false, error: String(error?.message ?? error) };
    }
  };
}

/** Everything is on until the person switches it off on the options page. */
async function capabilityEnabled(chrome, capability) {
  const { capabilities } = await chrome.storage.local.get("capabilities");
  return capabilities?.[capability] !== false;
}

/**
 * A tab that is still loading reports its address only as `pendingUrl`, and `url` is
 * empty. Saving the empty one would lose the tab, so every read of a URL comes here.
 */
function urlOf(tab) {
  return tab.url || tab.pendingUrl || "";
}

function compactTab(tab) {
  return {
    id: tab.id,
    windowId: tab.windowId,
    index: tab.index,
    title: tab.title ?? "",
    url: urlOf(tab),
    active: tab.active,
    pinned: tab.pinned,
    audible: tab.audible ?? false,
    discarded: tab.discarded,
    groupId: tab.groupId ?? NO_GROUP,
    lastAccessed: tab.lastAccessed ?? null,
  };
}

async function listTabs(chrome) {
  const [tabs, groups] = await Promise.all([chrome.tabs.query({}), chrome.tabGroups.query({})]);
  return {
    extensionId: chrome.runtime.id,
    tabs: tabs.map(compactTab),
    groups: groups.map(({ id, title, color, collapsed, windowId }) => ({
      id,
      title: title ?? "",
      color,
      collapsed,
      windowId,
    })),
  };
}

/** A protection named in `allow` is one the caller knowingly chose to override. */
function allowedProtections(request) {
  const allow = request.allow ?? [];
  if (!Array.isArray(allow) || allow.some((name) => !PROTECTIONS.includes(name))) {
    throw new Error("allow may only name active, pinned or audible");
  }
  return allow;
}

function targetsOf(request) {
  if (!Array.isArray(request.targets)) throw new Error("targets must be a list");
  return request.targets;
}

function refused(target, reason) {
  return { id: target?.id ?? null, status: "refused", reason };
}

function failed(id, error) {
  return { id, status: "failed", reason: String(error?.message ?? error) };
}

/**
 * The tab a target names, or the reason it must not be touched. The URL check is what
 * makes a plan made from a stale list fail safely: Chrome reuses nothing, but the page
 * in a tab changes while the person keeps browsing.
 */
async function resolveTarget(chrome, target, protections, allow) {
  if (!Number.isInteger(target?.id) || typeof target?.url !== "string") {
    return { refusal: refused(target, "bad_target") };
  }
  let tab;
  try {
    tab = await chrome.tabs.get(target.id);
  } catch {
    return { refusal: refused(target, "gone") };
  }
  if (urlOf(tab) !== target.url) return { refusal: refused(target, "url_changed") };
  for (const protection of protections) {
    if (tab[protection] && !allow.includes(protection)) {
      return { refusal: refused(target, protection) };
    }
  }
  return { tab };
}

/** Runs `act` on each target that passes its checks, giving every target its own result. */
async function forEachTarget(chrome, request, protections, act) {
  const allow = allowedProtections(request);
  const results = [];
  for (const target of targetsOf(request)) {
    const { refusal, tab } = await resolveTarget(chrome, target, protections, allow);
    if (refusal) {
      results.push(refusal);
      continue;
    }
    try {
      results.push({ id: target.id, status: "done", ...(await act(tab)) });
    } catch (error) {
      results.push(failed(target.id, error));
    }
  }
  return { results };
}

// Chrome discards an active tab if asked, so this protection exists only here.
async function discardTabs(chrome, request) {
  return forEachTarget(chrome, request, PROTECTIONS, async (tab) => {
    if (tab.discarded) throw new Error("already discarded");
    const discarded = await chrome.tabs.discard(tab.id);
    return { discarded: discarded?.discarded ?? true };
  });
}

async function closeTabs(chrome, request) {
  return forEachTarget(chrome, request, PROTECTIONS, async (tab) => {
    await chrome.tabs.remove(tab.id);
    return {};
  });
}

async function placement(chrome, id) {
  const tab = await chrome.tabs.get(id);
  return {
    windowId: tab.windowId,
    index: tab.index,
    pinned: tab.pinned,
    groupId: tab.groupId ?? NO_GROUP,
  };
}

/** Chrome adjusts what is asked of it, so results report where tabs ended up. */
async function withFinalPlacement(chrome, results) {
  return Promise.all(
    results.map(async (result) => {
      if (result.status !== "done") return result;
      try {
        return { ...result, ...(await placement(chrome, result.id)) };
      } catch (error) {
        return failed(result.id, error);
      }
    }),
  );
}

async function moveTabs(chrome, request) {
  if (!Number.isInteger(request.index) || request.index < -1) {
    throw new Error("index must be a position, or -1 for the end");
  }
  let next = request.index;
  const { results } = await forEachTarget(chrome, request, [], async (tab) => {
    const destination = { index: next };
    if (request.windowId !== undefined) destination.windowId = request.windowId;
    await chrome.tabs.move(tab.id, destination);
    if (next !== -1) next += 1;
    return { requestedIndex: destination.index };
  });
  return { results: await withFinalPlacement(chrome, results) };
}

// Grouping a pinned tab silently unpins it, and grouping across windows moves tabs into
// one window and closes a window it empties. Results report both.
async function groupTabs(chrome, request) {
  const allow = allowedProtections(request);
  const results = [];
  const accepted = [];
  for (const target of targetsOf(request)) {
    const { refusal, tab } = await resolveTarget(chrome, target, ["pinned"], allow);
    if (refusal) {
      results.push(refusal);
      continue;
    }
    accepted.push(tab);
    results.push({ id: tab.id, status: "done", fromWindowId: tab.windowId });
  }
  if (accepted.length === 0) return { groupId: null, results };

  const tabIds = accepted.map((tab) => tab.id);
  let groupId;
  try {
    groupId = await chrome.tabs.group(
      request.groupId === undefined ? { tabIds } : { tabIds, groupId: request.groupId },
    );
    const properties = {};
    if (request.title !== undefined) properties.title = request.title;
    if (request.color !== undefined) properties.color = request.color;
    if (Object.keys(properties).length > 0) await chrome.tabGroups.update(groupId, properties);
  } catch (error) {
    return {
      groupId: groupId ?? null,
      results: results.map((result) => (result.status === "done" ? failed(result.id, error) : result)),
    };
  }
  return { groupId, results: await withFinalPlacement(chrome, results) };
}

async function ungroupTabs(chrome, request) {
  const { results } = await forEachTarget(chrome, request, [], async (tab) => {
    await chrome.tabs.ungroup(tab.id);
    return {};
  });
  return { results: await withFinalPlacement(chrome, results) };
}

/** `javascript:` would run code in whatever page is current. Nothing else needs refusing. */
function isOpenable(url) {
  return typeof url === "string" && url.length > 0 && !/^\s*javascript:/i.test(url);
}

/**
 * Reopens saved tabs. Tab, window and group IDs do not survive a browser restart, so a
 * saved window is reused only if it is still open, and every group is created anew.
 */
async function restoreTabs(chrome, request) {
  if (!Array.isArray(request.tabs)) throw new Error("tabs must be a list");
  const results = new Map();
  const openedByWindow = new Map();
  const openedByGroup = new Map();
  const windows = new Map();
  const ordered = request.tabs
    .map((tab, position) => ({ tab, position }))
    .sort((a, b) => (a.tab.index ?? 0) - (b.tab.index ?? 0) || a.position - b.position);

  for (const { tab, position } of ordered) {
    if (!isOpenable(tab.url)) {
      results.set(position, { position, status: "refused", reason: "not_openable" });
      continue;
    }
    try {
      const opened = await openTab(chrome, tab, windows);
      results.set(position, { position, status: "done", id: opened.id });
      if (tab.group !== null && tab.group !== undefined) {
        const members = openedByGroup.get(tab.group) ?? [];
        members.push(opened);
        openedByGroup.set(tab.group, members);
      }
      openedByWindow.set(opened.windowId, true);
    } catch (error) {
      results.set(position, { position, ...failed(null, error), id: undefined });
    }
  }

  const groupProblems = await recreateGroups(chrome, request.groups ?? [], openedByGroup);
  const finished = await withFinalPlacement(
    chrome,
    [...results.values()].sort((a, b) => a.position - b.position),
  );
  return { results: finished, groupProblems };
}

async function openTab(chrome, tab, windows) {
  const windowId = await restoredWindow(chrome, tab.window, windows);
  if (windowId === null) {
    const created = await chrome.windows.create({ url: tab.url, focused: false });
    windows.set(tab.window, created.id);
    const first = created.tabs[0];
    if (tab.pinned) await chrome.tabs.update(first.id, { pinned: true });
    return first;
  }
  const properties = { windowId, url: tab.url, active: false, pinned: Boolean(tab.pinned) };
  if (Number.isInteger(tab.index)) properties.index = tab.index;
  return chrome.tabs.create(properties);
}

/** The window this saved window now is, or null if a new one is needed. */
async function restoredWindow(chrome, saved, windows) {
  if (windows.has(saved)) return windows.get(saved);
  try {
    const existing = await chrome.windows.get(saved);
    windows.set(saved, existing.id);
    return existing.id;
  } catch {
    return null;
  }
}

async function recreateGroups(chrome, groups, openedByGroup) {
  const problems = [];
  for (const group of groups) {
    const members = openedByGroup.get(group.key);
    if (!members) continue;
    try {
      const groupId = await chrome.tabs.group({
        tabIds: members.map((tab) => tab.id),
        createProperties: { windowId: members[0].windowId },
      });
      await chrome.tabGroups.update(groupId, {
        title: group.title ?? "",
        ...(group.color ? { color: group.color } : {}),
        collapsed: Boolean(group.collapsed),
      });
    } catch (error) {
      problems.push({ key: group.key, reason: String(error?.message ?? error) });
    }
  }
  return problems;
}

async function listBookmarks(chrome, request) {
  const tree =
    request.nodeId === undefined ? await chrome.bookmarks.getTree() : await chrome.bookmarks.getSubTree(request.nodeId);
  return { tree };
}

async function addBookmarks(chrome, request) {
  if (!Array.isArray(request.items)) throw new Error("items must be a list");
  let parentId = request.parentId;
  let folderId = null;
  if (request.folder !== undefined) {
    const folder = await chrome.bookmarks.create({
      title: request.folder,
      ...(parentId === undefined ? {} : { parentId }),
    });
    parentId = folder.id;
    folderId = folder.id;
  }
  const results = [];
  for (const item of request.items) {
    if (!isOpenable(item.url)) {
      results.push({ status: "refused", reason: "not_openable", title: item.title });
      continue;
    }
    try {
      const created = await chrome.bookmarks.create({
        title: item.title,
        url: item.url,
        ...(parentId === undefined ? {} : { parentId }),
      });
      results.push({ status: "done", bookmarkId: created.id });
    } catch (error) {
      results.push(failed(null, error));
    }
  }
  return { folderId, results };
}

/** The caller saves the subtree it was shown; the version must still match. */
async function removeBookmark(chrome, request) {
  const [node] = await chrome.bookmarks.getSubTree(request.nodeId);
  if (request.expectedTitle !== undefined && node.title !== request.expectedTitle) {
    throw new Error("the bookmark changed since it was read");
  }
  if (node.url === undefined) {
    await chrome.bookmarks.removeTree(node.id);
  } else {
    await chrome.bookmarks.remove(node.id);
  }
  return { removed: node.id };
}

async function restoreBookmarks(chrome, request) {
  const restore = async (node, parentId, index) => {
    const created = await chrome.bookmarks.create({
      title: node.title,
      ...(node.url === undefined ? {} : { url: node.url }),
      ...(parentId === undefined ? {} : { parentId }),
      ...(index === undefined ? {} : { index }),
    });
    for (const child of node.children ?? []) await restore(child, created.id);
    return created.id;
  };
  return { bookmarkId: await restore(request.node, request.parentId, request.index) };
}
