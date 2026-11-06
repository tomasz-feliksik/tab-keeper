// Connects this profile's tabs to the tab-keeper host over native messaging. The
// host owns the socket that clients use; this side only answers, and it is the last
// place a limit is enforced.
import { createAnswerer } from "./core.js";

const HOST_NAME = "io.github.tomasz_feliksik.tab_keeper";

const answer = createAnswerer(chrome);
let port = null;
let connecting = false;

async function instanceId() {
  const stored = await chrome.storage.local.get("instance");
  if (stored.instance) return stored.instance;
  const instance = crypto.randomUUID().replaceAll("-", "");
  await chrome.storage.local.set({ instance });
  return instance;
}

async function connect() {
  if (port !== null || connecting) return;
  connecting = true;
  try {
    const instance = await instanceId();
    const opened = chrome.runtime.connectNative(HOST_NAME);
    opened.onDisconnect.addListener(() => {
      // Reading lastError marks it handled. A profile whose user has no native host
      // lands here on every alarm, and that is expected.
      void chrome.runtime.lastError;
      port = null;
    });
    opened.onMessage.addListener(async (request) => {
      opened.postMessage(await answer(request));
    });
    opened.postMessage({ type: "hello", instance });
    port = opened;
  } finally {
    connecting = false;
  }
}

// The alarm brings the connection back after the host dies or the worker is evicted.
chrome.alarms.create("reconnect", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();
