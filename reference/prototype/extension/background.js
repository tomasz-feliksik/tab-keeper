// Bridges this profile's tabs to the chrome-tabs CLI over native messaging.
// The native host owns the socket the CLI connects to; this side only answers.
const HOST_NAME = "org.tomasz.chrome_tabs";

let port = null;
let connecting = false;

async function instanceId() {
  const stored = await chrome.storage.local.get("instance");
  if (stored.instance) return stored.instance;
  const instance = crypto.randomUUID().replaceAll("-", "");
  await chrome.storage.local.set({ instance });
  return instance;
}

async function listTabs() {
  const tabs = await chrome.tabs.query({});
  return tabs.map((tab) => ({
    id: tab.id,
    windowId: tab.windowId,
    title: tab.title ?? "",
    url: tab.url ?? "",
    active: tab.active,
    pinned: tab.pinned,
    audible: tab.audible ?? false,
    discarded: tab.discarded,
    lastAccessed: tab.lastAccessed ?? null,
  }));
}

async function discardTabs(tabIds) {
  const discarded = [];
  const failed = [];
  for (const id of tabIds) {
    try {
      await chrome.tabs.discard(id);
      discarded.push(id);
    } catch (error) {
      failed.push({ id, error: String(error) });
    }
  }
  return { discarded, failed };
}

async function answer(request) {
  if (request.op === "list") {
    return { extensionId: chrome.runtime.id, tabs: await listTabs() };
  }
  if (request.op === "discard") {
    return await discardTabs(request.tabIds);
  }
  throw new Error(`unknown op: ${request.op}`);
}

async function connect() {
  if (port !== null || connecting) return;
  connecting = true;
  try {
    const instance = await instanceId();
    const opened = chrome.runtime.connectNative(HOST_NAME);
    opened.onDisconnect.addListener(() => {
      // Reading lastError marks it handled. A profile whose user has no native
      // host lands here on every alarm, and that is expected.
      void chrome.runtime.lastError;
      port = null;
    });
    opened.onMessage.addListener(async (request) => {
      try {
        opened.postMessage({ id: request.id, ok: true, result: await answer(request) });
      } catch (error) {
        opened.postMessage({ id: request.id, ok: false, error: String(error) });
      }
    });
    opened.postMessage({ type: "hello", instance });
    port = opened;
  } finally {
    connecting = false;
  }
}

// An open native port keeps this service worker alive; the alarm brings the
// bridge back after the host dies or the worker is evicted anyway.
chrome.alarms.create("reconnect", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();
