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

// Chrome holds back an update while the extension is running, and the open port keeps it
// running for as long as the browser does. Reloading applies it; the alarm reconnects.
chrome.runtime.onUpdateAvailable.addListener(() => chrome.runtime.reload());

// Chrome does not look for an update when the policy changes, only every few hours or when
// it starts, so a machine switched to a new release would keep the old code. The update
// manifest is a local file, so asking often costs nothing. A throttled check waits for the next.
const UPDATE_CHECK_MS = 10 * 60 * 1000;
let lastUpdateCheck = Date.now();
async function checkForUpdate() {
  if (Date.now() - lastUpdateCheck < UPDATE_CHECK_MS) return;
  lastUpdateCheck = Date.now();
  try {
    await chrome.runtime.requestUpdateCheck();
  } catch {
    // Reported again at the next check; there is nobody to tell here.
  }
}

// The alarm brings the connection back after the host dies or the worker is evicted.
chrome.alarms.create("reconnect", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener(connect);
chrome.alarms.onAlarm.addListener(checkForUpdate);
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();
