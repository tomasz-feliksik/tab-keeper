// Real Chromium, the real extension, the real host and the real CLI, in throwaway
// profiles. The traps this avoids are in docs/things-to-keep-in-mind.md, Testing traps.

import { spawn, spawnSync } from "node:child_process";
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const BINARY = join(ROOT, "target", "debug", "tab-keeper");
export const EXTENSION_ID = "niglgkgdkfompjmcbpldgepicfnjlfnc";
const HOST_NAME = "io.github.tomasz_feliksik.tab_keeper";
const EXTENSION_FILES = ["manifest.json", "background.js", "core.js", "options.html", "options.js"];
const PATIENCE_MS = 30_000;

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export async function eventually(what, check) {
  const deadline = Date.now() + PATIENCE_MS;
  let last;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      last = error;
    }
    await sleep(100);
  }
  throw new Error(`gave up waiting for ${what}${last ? `: ${last.message}` : ""}`);
}

function chromiumPath() {
  const browsers = process.env.PLAYWRIGHT_BROWSERS_PATH;
  if (!browsers) throw new Error("no PLAYWRIGHT_BROWSERS_PATH: run this through `just e2e`");
  const chromium = readdirSync(browsers).find((name) => /^chromium-\d+$/.test(name));
  if (!chromium) throw new Error(`no Chromium under ${browsers}`);
  return join(browsers, chromium, "chrome-linux64", "chrome");
}

/** Pages with a known title, on loopback, so nothing here needs the network. */
export async function startPages() {
  const server = createServer((request, response) => {
    const title = new URL(request.url, "http://x").searchParams.get("title") ?? "page";
    response.setHeader("content-type", "text/html; charset=utf-8");
    response.end(`<!doctype html><title>${title.replace(/</g, "&lt;")}</title><p>page</p>`);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  return {
    url: (path, title = path) => `${base}/${path}?title=${encodeURIComponent(title)}`,
    stop: () => server.close(),
  };
}

class Cdp {
  constructor(socket) {
    this.socket = socket;
    this.sent = 0;
    this.waiting = new Map();
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const waiter = this.waiting.get(message.id);
      if (!waiter) return;
      this.waiting.delete(message.id);
      if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
      else waiter.resolve(message.result);
    });
  }

  static async open(url) {
    const socket = new WebSocket(url);
    await new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve);
      socket.addEventListener("error", reject);
    });
    return new Cdp(socket);
  }

  send(method, params = {}) {
    const id = ++this.sent;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  close() {
    this.socket.close();
  }
}

export class Browser {
  constructor(world, directory) {
    this.world = world;
    this.directory = directory;
    this.userData = join(world.root, directory);
    mkdirSync(join(this.userData, "NativeMessagingHosts"), { recursive: true });
    writeFileSync(
      join(this.userData, "NativeMessagingHosts", `${HOST_NAME}.json`),
      JSON.stringify({
        name: HOST_NAME,
        description: "tab-keeper",
        path: world.hostWrapper,
        type: "stdio",
        allowed_origins: [`chrome-extension://${EXTENSION_ID}/`],
      }),
    );
  }

  /** The debugging port is chosen by Chromium, since a fixed one can already be taken. */
  async start() {
    rmSync(join(this.userData, "DevToolsActivePort"), { force: true });
    const env = { ...this.world.env };
    // The dev shell's libraries break Chromium.
    delete env.LD_LIBRARY_PATH;
    this.process = spawn(
      chromiumPath(),
      [
        `--user-data-dir=${this.userData}`,
        `--load-extension=${this.world.extension}`,
        `--disable-extensions-except=${this.world.extension}`,
        "--remote-debugging-port=0",
        "--no-sandbox",
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--restore-last-session",
        "about:blank",
      ],
      { env, stdio: "ignore" },
    );
    const port = await eventually(
      "the debugging port",
      () => readFileSync(join(this.userData, "DevToolsActivePort"), "utf8").split("\n")[0],
    );
    this.port = port;
    this.worker = await eventually("the extension's service worker", async () => {
      const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      // Chromium has service workers of its own, so match on this extension's ID.
      const target = targets.find(
        (t) => t.type === "service_worker" && t.url.startsWith(`chrome-extension://${EXTENSION_ID}/`),
      );
      return target ? Cdp.open(target.webSocketDebuggerUrl) : null;
    });
    await eventually("the host's socket", () => this.instanceSocket());
    return this;
  }

  async instance() {
    return this.evaluate(`return (await chrome.storage.local.get("instance")).instance`);
  }

  async instanceSocket() {
    const instance = await this.instance();
    if (!instance) return null;
    const sockets = readdirSync(this.world.socketDirectory, { throwIfNoEntry: false }) ?? [];
    return sockets.includes(`${instance}.sock`) ? join(this.world.socketDirectory, `${instance}.sock`) : null;
  }

  /** Runs inside the extension's worker, which is how a test arranges the browser. */
  async evaluate(body) {
    const answer = await this.worker.send("Runtime.evaluate", {
      expression: `(async () => JSON.stringify(await (async () => { ${body} })()))()`,
      awaitPromise: true,
      returnByValue: true,
    });
    if (answer.exceptionDetails) {
      throw new Error(answer.exceptionDetails.exception?.description ?? answer.exceptionDetails.text);
    }
    return answer.result.value === undefined ? undefined : JSON.parse(answer.result.value);
  }

  /** Opens a tab and waits until its page has loaded, so its title and URL are settled. */
  async open(url, properties = {}) {
    const created = await this.evaluate(
      `return await chrome.tabs.create(${JSON.stringify({ url, active: false, ...properties })})`,
    );
    await eventually(`the tab for ${url}`, async () => {
      const tab = await this.evaluate(`return await chrome.tabs.get(${created.id})`);
      return tab.status === "complete" && tab.url === url;
    });
    return created.id;
  }

  tabs() {
    return this.evaluate(`return await chrome.tabs.query({})`);
  }

  /** A tab opened by a restore reports its URL only once navigation has begun. */
  async tabAt(url) {
    return eventually(`a tab at ${url}`, async () => (await this.tabs()).find((tab) => tab.url === url));
  }

  async stop() {
    this.worker?.close();
    if (!this.process || this.process.exitCode !== null) return;
    const exited = new Promise((resolve) => this.process.once("exit", resolve));
    this.process.kill("SIGTERM");
    await exited;
  }
}

export class World {
  constructor() {
    // A socket path has to fit about 108 bytes, so this is short on purpose.
    this.root = mkdtempSync(join(tmpdir(), "tk"));
    this.runtime = join(this.root, "run");
    this.state = join(this.root, "state");
    this.socketDirectory = join(this.runtime, "tab-keeper");
    this.policy = join(this.root, "policy");
    this.extension = join(this.root, "extension");
    this.hostWrapper = join(this.root, "host-wrapper");
    mkdirSync(this.runtime, { mode: 0o700 });
    mkdirSync(this.policy);
    mkdirSync(this.extension);
    for (const file of EXTENSION_FILES) cpSync(join(ROOT, "extension", file), join(this.extension, file));
    writeFileSync(join(this.policy, "policy.json"), JSON.stringify({ TabDiscardingExceptions: [] }));
    writeFileSync(this.hostWrapper, `#!/bin/sh\nexec ${JSON.stringify(BINARY)} host "$@"\n`);
    chmodSync(this.hostWrapper, 0o755);
    this.browsers = [];
    this.env = {
      ...process.env,
      XDG_RUNTIME_DIR: this.runtime,
      XDG_STATE_HOME: this.state,
      TAB_KEEPER_BROWSER_DIRS: "",
    };
  }

  browser(directory) {
    const browser = new Browser(this, directory);
    this.browsers.push(browser);
    this.env.TAB_KEEPER_BROWSER_DIRS = this.browsers.map((b) => b.userData).join(":");
    return browser;
  }

  setPolicy(policy) {
    writeFileSync(join(this.policy, "policy.json"), JSON.stringify(policy));
  }

  /** The CLI's status and JSON. Never throws on a non-zero status: tests assert it. */
  cli(...args) {
    const result = spawnSync(BINARY, ["--json", "--timeout", "10", ...args], { env: this.env, encoding: "utf8" });
    let json = null;
    try {
      json = JSON.parse(result.stdout);
    } catch {
      // A refused command prints only to stderr.
    }
    return { status: result.status, json, stderr: result.stderr, stdout: result.stdout };
  }

  /** One line per message, as a harness speaks it. */
  mcp(...messages) {
    const result = spawnSync(BINARY, ["--timeout", "10", "mcp", "--policy-dir", this.policy], {
      env: this.env,
      encoding: "utf8",
      input: messages.map((message) => `${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`).join(""),
    });
    return result.stdout
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  }

  /**
   * The process of this world's own host, found by its environment. Matching on a command
   * line would also hit any other process that merely mentions it, a shell included.
   */
  hostPid() {
    for (const entry of readdirSync("/proc")) {
      if (!/^\d+$/.test(entry)) continue;
      try {
        const isHost = readFileSync(`/proc/${entry}/cmdline`, "utf8").split("\0")[1] === "host";
        const environment = readFileSync(`/proc/${entry}/environ`, "utf8").split("\0");
        if (isHost && environment.includes(`XDG_RUNTIME_DIR=${this.runtime}`)) return Number(entry);
      } catch {
        // It exited, or belongs to someone else.
      }
    }
    return null;
  }

  /** The reference `list` gives the tab with this URL, in this browser. */
  async refFor(url) {
    const listed = this.cli("list", "--limit", "1000", "--full");
    const tab = listed.json?.tabs.find((candidate) => candidate.url === url);
    if (!tab) throw new Error(`no tab listed at ${url}: ${listed.stdout}${listed.stderr}`);
    return tab.ref;
  }

  async stop() {
    for (const browser of this.browsers) await browser.stop();
    rmSync(this.root, { recursive: true, force: true });
  }
}
