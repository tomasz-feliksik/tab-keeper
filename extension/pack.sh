#!/usr/bin/env bash
# Signs the extension: writes extension/tab-keeper.crx from the source files beside this
# script. Needs Chromium and sops, as in `nix develop .#e2e`, and an age identity that the
# signing key is encrypted to. The key is decrypted to a private temporary directory, in
# memory where possible, for as long as the pack takes.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
readonly files=(manifest.json background.js core.js options.html options.js)

: "${PLAYWRIGHT_BROWSERS_PATH:?run this through: nix develop .#e2e --command just pack}"
chromium=("$PLAYWRIGHT_BROWSERS_PATH"/chromium-[0-9]*/chrome-linux64/chrome)
[[ -x "${chromium[0]}" ]] || {
  echo "no Chromium under $PLAYWRIGHT_BROWSERS_PATH" >&2
  exit 1
}

memory=/dev/shm
[[ -d "$memory" && -w "$memory" ]] || memory=${XDG_RUNTIME_DIR:-/tmp}
work=$(mktemp -d -p "$memory" tab-keeper-pack.XXXXXX)
cleanup() {
  [[ ! -e "$work/key.pem" ]] || shred -u "$work/key.pem"
  rm -rf "$work"
  # A Chromium helper can outlive the pack and put its profile back.
  sleep 1
  rm -rf "$work"
}
trap cleanup EXIT
chmod 700 "$work"

sops decrypt --input-type binary --output-type binary \
  --output "$work/key.pem" "$here/signing-key.sops.json"

mkdir "$work/tab-keeper"
for file in "${files[@]}"; do
  cp "$here/$file" "$work/tab-keeper/$file"
done
# A checkout's timestamps should not be able to change the package.
find "$work/tab-keeper" -exec touch --date=@0 {} +

# A fresh profile keeps the pack from being handed to a running browser. Chromium exits
# non-zero here even when it succeeds, so the file is what gets checked.
env -u LD_LIBRARY_PATH "${chromium[0]}" --user-data-dir="$work/profile" --no-first-run --no-sandbox \
  --headless=new --disable-gpu --disable-crash-reporter --disable-breakpad --pack-extension="$work/tab-keeper" --pack-extension-key="$work/key.pem" \
  >"$work/pack.log" 2>&1 || true
[[ -s "$work/tab-keeper.crx" ]] || {
  echo "Chromium did not write a package:" >&2
  cat "$work/pack.log" >&2
  exit 1
}
install -m 644 "$work/tab-keeper.crx" "$here/tab-keeper.crx"
echo "wrote extension/tab-keeper.crx"
