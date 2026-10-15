#!/usr/bin/env node
/** Validate the Windows .ico directory, including the little-endian offsets used by RC.EXE. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const bytes = readFileSync(process.argv[2] ?? new URL('../src-tauri/icons/icon.ico', import.meta.url));
assert.equal(bytes.readUInt16LE(0), 0, 'ICO reserved field');
assert.equal(bytes.readUInt16LE(2), 1, 'ICO type must be icon');
const count = bytes.readUInt16LE(4);
assert.ok(count > 0 && count <= 32, 'plausible icon count');

const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
let nextOffset = 6 + count * 16;
for (let index = 0; index < count; index += 1) {
  const entry = 6 + index * 16;
  const width = bytes[entry] || 256;
  const height = bytes[entry + 1] || 256;
  const length = bytes.readUInt32LE(entry + 8);
  const offset = bytes.readUInt32LE(entry + 12);
  assert.equal(width, height, `icon ${index} must be square`);
  assert.ok(length >= 24, `icon ${index} has an empty image`);
  assert.equal(offset, nextOffset, `icon ${index} has an incorrect little-endian offset`);
  assert.ok(offset + length <= bytes.length, `icon ${index} extends beyond the ICO`);
  assert.deepEqual(bytes.subarray(offset, offset + 8), pngSignature, `icon ${index} lacks a PNG`);
  assert.equal(bytes.toString('ascii', offset + 12, offset + 16), 'IHDR');
  assert.equal(bytes.readUInt32BE(offset + 16), width, `icon ${index} PNG width mismatch`);
  assert.equal(bytes.readUInt32BE(offset + 20), height, `icon ${index} PNG height mismatch`);
  nextOffset += length;
}
assert.equal(nextOffset, bytes.length, 'ICO has unreferenced trailing data');

// The macOS bundle references icon.icns as well. It must be a real ICNS container, not a PNG
// renamed to .icns. This verifies format structure only, not a native macOS installer.
const icns = readFileSync(new URL('../src-tauri/icons/icon.icns', import.meta.url));
assert.equal(icns.toString('ascii', 0, 4), 'icns');
assert.equal(icns.readUInt32BE(4), icns.length, 'ICNS total length');
assert.equal(icns.toString('ascii', 8, 12), 'ic09', '512px icon type');
assert.equal(icns.readUInt32BE(12), icns.length - 8, 'ICNS image entry length');
assert.deepEqual(icns.subarray(16, 24), pngSignature, 'ICNS entry must contain a PNG');
assert.equal(icns.readUInt32BE(32), 512, 'ICNS PNG width');
assert.equal(icns.readUInt32BE(36), 512, 'ICNS PNG height');

console.log(`Verified ${count} Windows ICO images and the ICNS container structure.`);
