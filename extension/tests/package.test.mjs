// Holds the committed package to its source, without needing the private key. A package
// that lags its source would have tests pass against one extension while people run another.

import assert from "node:assert/strict";
import { createHash, createPublicKey, verify } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { inflateRawSync } from "node:zlib";

const EXTENSION = join(dirname(fileURLToPath(import.meta.url)), "..");
const EXTENSION_ID = "niglgkgdkfompjmcbpldgepicfnjlfnc";
const FILES = ["background.js", "core.js", "manifest.json", "options.html", "options.js"];
const CRX_MAGIC = "Cr24";
const CRX_VERSION = 3;
const SIGNATURE_PREFIX = "CRX3 SignedData\0";
const PROOF_FIELD_RSA = 2;
const SIGNED_DATA_FIELD = 10000;
const CRX_ID_FIELD = 1;

function readVarint(bytes, start) {
  let value = 0;
  let scale = 1;
  let at = start;
  for (;;) {
    const byte = bytes[at++];
    value += (byte & 0x7f) * scale;
    if (byte < 0x80) return { value, at };
    scale *= 128;
  }
}

/** The length-delimited fields of a protobuf message, which is all CRX3 uses. */
function protobufFields(bytes) {
  const fields = [];
  let at = 0;
  while (at < bytes.length) {
    const tag = readVarint(bytes, at);
    const length = readVarint(bytes, tag.at);
    assert.equal(tag.value & 7, 2, "CRX3 headers hold only length-delimited fields");
    fields.push({ number: tag.value >> 3, value: bytes.subarray(length.at, length.at + length.value) });
    at = length.at + length.value;
  }
  return fields;
}

function parseCrx(bytes) {
  assert.equal(bytes.subarray(0, 4).toString(), CRX_MAGIC, "not a CRX file");
  assert.equal(bytes.readUInt32LE(4), CRX_VERSION);
  const headerLength = bytes.readUInt32LE(8);
  const fields = protobufFields(bytes.subarray(12, 12 + headerLength));
  const signedData = fields.find((field) => field.number === SIGNED_DATA_FIELD).value;
  const proofs = fields.filter((field) => field.number === PROOF_FIELD_RSA).map((field) => protobufFields(field.value));
  const crxId = protobufFields(signedData).find((field) => field.number === CRX_ID_FIELD).value;
  return {
    signedData,
    crxId,
    zip: bytes.subarray(12 + headerLength),
    proofs: proofs.map((proof) => ({
      publicKey: proof.find((field) => field.number === 1).value,
      signature: proof.find((field) => field.number === 2).value,
    })),
  };
}

function readZip(zip) {
  const END_OF_DIRECTORY = 0x06054b50;
  const END_LENGTH = 22;
  const STORED = 0;
  let end = zip.length - END_LENGTH;
  while (end >= 0 && zip.readUInt32LE(end) !== END_OF_DIRECTORY) end--;
  assert.ok(end >= 0, "the package holds no zip");
  const count = zip.readUInt16LE(end + 10);
  let at = zip.readUInt32LE(end + 16);
  const files = new Map();
  for (let index = 0; index < count; index++) {
    const method = zip.readUInt16LE(at + 10);
    const compressedSize = zip.readUInt32LE(at + 20);
    const nameLength = zip.readUInt16LE(at + 28);
    const extraLength = zip.readUInt16LE(at + 30);
    const commentLength = zip.readUInt16LE(at + 32);
    const localHeader = zip.readUInt32LE(at + 42);
    const name = zip.subarray(at + 46, at + 46 + nameLength).toString();
    const dataStart = localHeader + 30 + zip.readUInt16LE(localHeader + 26) + zip.readUInt16LE(localHeader + 28);
    const raw = zip.subarray(dataStart, dataStart + compressedSize);
    files.set(name, method === STORED ? raw : inflateRawSync(raw));
    at += 46 + nameLength + extraLength + commentLength;
  }
  return files;
}

function idFromPublicKey(der) {
  const hex = createHash("sha256").update(der).digest("hex").slice(0, 32);
  return [...hex].map((digit) => String.fromCharCode(97 + Number.parseInt(digit, 16))).join("");
}

const crx = () => parseCrx(readFileSync(join(EXTENSION, "tab-keeper.crx")));
const manifest = () => JSON.parse(readFileSync(join(EXTENSION, "manifest.json"), "utf8"));

test("the package holds exactly the source, so it cannot lag behind it", () => {
  const files = readZip(crx().zip);

  assert.deepEqual([...files.keys()].sort(), FILES);
  for (const name of FILES) {
    assert.ok(
      files.get(name).equals(readFileSync(join(EXTENSION, name))),
      `${name} differs from the package: run \`nix develop .#e2e --command just pack\``,
    );
  }
});

test("the manifest's key produces the extension ID everything else pins", () => {
  assert.equal(idFromPublicKey(Buffer.from(manifest().key, "base64")), EXTENSION_ID);
});

test("the package is signed by the key in the manifest and claims the same ID", () => {
  const { signedData, crxId, proofs, zip } = crx();
  const key = Buffer.from(manifest().key, "base64");
  const proof = proofs.find((candidate) => candidate.publicKey.equals(key));
  assert.ok(proof, "no signature from the manifest's key");

  assert.equal(idFromPublicKey(Buffer.concat([key])), EXTENSION_ID);
  assert.deepEqual(crxId, createHash("sha256").update(key).digest().subarray(0, 16));

  const length = Buffer.alloc(4);
  length.writeUInt32LE(signedData.length);
  const signed = Buffer.concat([Buffer.from(SIGNATURE_PREFIX), length, signedData, zip]);
  const publicKey = createPublicKey({ key, format: "der", type: "spki" });
  assert.ok(verify("RSA-SHA256", signed, publicKey, proof.signature), "the signature does not verify");

  const tampered = Buffer.from(signed);
  tampered[tampered.length - 1] ^= 1;
  assert.ok(!verify("RSA-SHA256", tampered, publicKey, proof.signature), "the check itself can fail");
});
