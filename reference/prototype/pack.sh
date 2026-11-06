#!/usr/bin/env bash
# Repack the extension after changing src/: writes chrome-tabs.crx, the manifest's
# public key and _id.nix. Every run mints a new key on purpose: the ID derives from
# it, the extension holds no state worth keeping across an ID change, and so no
# private key has to live anywhere. Policy and the native host read _id.nix.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$work/key.pem" 2>/dev/null
openssl pkey -in "$work/key.pem" -pubout -outform DER -out "$work/pub.der"

python3 - "$here/src/manifest.json" "$work/pub.der" "$here/_id.nix" <<'PY'
import base64, hashlib, json, sys
from pathlib import Path
manifest_path, der_path, id_path = map(Path, sys.argv[1:])
der = der_path.read_bytes()
manifest = json.loads(manifest_path.read_text())
manifest["key"] = base64.b64encode(der).decode()
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
ext_id = "".join(chr(ord("a") + int(c, 16)) for c in hashlib.sha256(der).hexdigest()[:32])
id_path.write_text(f'"{ext_id}"\n')
PY

cp -r "$here/src" "$work/chrome-tabs"
# A fresh user-data-dir keeps the pack from handing off to a running Chrome.
google-chrome-stable --user-data-dir="$work/profile" --no-first-run --headless=new \
  --pack-extension="$work/chrome-tabs" --pack-extension-key="$work/key.pem" >/dev/null 2>&1 || true
mv "$work/chrome-tabs.crx" "$here/chrome-tabs.crx"
echo "packed $(cat "$here/_id.nix")"
