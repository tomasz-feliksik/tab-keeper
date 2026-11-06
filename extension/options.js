import { CAPABILITIES } from "./core.js";

const form = document.getElementById("capabilities");
const { capabilities = {} } = await chrome.storage.local.get("capabilities");

for (const capability of CAPABILITIES) {
  const label = document.createElement("label");
  const box = document.createElement("input");
  box.type = "checkbox";
  box.name = capability;
  box.checked = capabilities[capability] !== false;
  box.addEventListener("change", async () => {
    capabilities[capability] = box.checked;
    await chrome.storage.local.set({ capabilities });
  });
  label.append(box, ` ${capability}`);
  const line = document.createElement("div");
  line.append(label);
  form.append(line);
}
