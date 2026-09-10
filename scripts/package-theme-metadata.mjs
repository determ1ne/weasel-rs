import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { randomUUID } from 'node:crypto';

export const METADATA_LIMIT = 1024 * 1024;
export const WASM_LIMIT = 32 * 1024 * 1024;
const header = Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]);
const sectionName = Buffer.from('weasel.settings');

// Read at most limit + 1 bytes, even if a source grows after opening.
function readBounded(file, limit) {
  const fd = fs.openSync(file, 'r');
  try {
    if (!fs.fstatSync(fd).isFile()) throw new Error(`Not a regular file: ${file}`);
    const buffer = Buffer.alloc(limit + 1);
    let size = 0;
    while (size < buffer.length) {
      const count = fs.readSync(fd, buffer, size, buffer.length - size, null);
      if (!count) break;
      size += count;
    }
    if (size > limit) throw new Error(`Size limit exceeded: ${file}`);
    return buffer.subarray(0, size);
  } finally { fs.closeSync(fd); }
}

export function encodeU32(value) {
  const bytes = [];
  do {
    const byte = value % 128;
    value = Math.floor(value / 128);
    bytes.push(byte | (value ? 128 : 0));
  } while (value);
  return Buffer.from(bytes);
}

function readU32(bytes, cursor, end) {
  let value = 0;
  for (let i = 0; i < 5; i++) {
    if (cursor.offset >= end) throw new Error('Truncated WASM u32');
    const byte = bytes[cursor.offset++];
    if (i === 4 && byte > 15) throw new Error('Overflowing WASM u32');
    value += (byte & 127) * 2 ** (7 * i);
    if (!(byte & 128)) return value;
  }
  throw new Error('Invalid WASM u32');
}

// Only inspect section framing; never compile, instantiate, or execute a guest.
export function embedMetadata(wasm, metadata) {
  if (metadata.length > METADATA_LIMIT) throw new Error('Metadata exceeds 1 MiB');
  if (wasm.length > WASM_LIMIT) throw new Error('WASM exceeds 32 MiB');
  if (!wasm.subarray(0, 8).equals(header)) throw new Error('Invalid WASM header/version');
  const parts = [wasm.subarray(0, 8)];
  const cursor = { offset: 8 };
  let size = 8;
  while (cursor.offset < wasm.length) {
    const start = cursor.offset;
    const id = wasm[cursor.offset++];
    if (id > 13) throw new Error('Invalid WASM section ID');
    const length = readU32(wasm, cursor, wasm.length);
    const end = cursor.offset + length;
    if (end > wasm.length) throw new Error('Truncated WASM section');
    let replace = false;
    if (id === 0) {
      const nameLength = readU32(wasm, cursor, end);
      const nameEnd = cursor.offset + nameLength;
      if (nameEnd > end) throw new Error('Truncated WASM custom section name');
      const name = wasm.subarray(cursor.offset, nameEnd);
      new TextDecoder('utf-8', { fatal: true }).decode(name);
      replace = name.equals(sectionName);
    }
    if (!replace) {
      parts.push(wasm.subarray(start, end));
      size += end - start;
    }
    cursor.offset = end;
  }
  const payload = Buffer.concat([encodeU32(sectionName.length), sectionName, metadata]);
  const section = Buffer.concat([Buffer.from([0]), encodeU32(payload.length), payload]);
  if (size + section.length > WASM_LIMIT) throw new Error('Packaged WASM exceeds 32 MiB');
  return Buffer.concat([...parts, section], size + section.length);
}

function readJson(file) {
  return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(readBounded(file, METADATA_LIMIT)));
}

export function packageMetadata(source) {
  if (!source.endsWith('.json')) throw new Error('Source config must end in .json');
  // JSON data only: references are preserved verbatim, never resolved or fetched.
  const defaults = readJson(source);
  const richschema = readJson(source.slice(0, -5) + '.richschema.json');
  const bytes = Buffer.from(JSON.stringify({ formatVersion: 1, defaults, richschema }));
  if (bytes.length > METADATA_LIMIT) throw new Error('Metadata exceeds 1 MiB');
  return bytes;
}

function atomicWrite(file, bytes) {
  const temporary = path.join(path.dirname(file), `.${path.basename(file)}.${randomUUID()}.tmp`);
  try {
    fs.writeFileSync(temporary, bytes, { flag: 'wx' });
    fs.renameSync(temporary, file);
  } finally {
    if (fs.existsSync(temporary)) fs.unlinkSync(temporary);
  }
}

export function main(args) {
  if (args.length !== 3 || !['--native', '--wasm'].includes(args[0])) {
    throw new Error('Usage: --native source-config-path output.settings.json OR --wasm module-path source-config-path');
  }
  const [mode, first, second] = args;
  const source = mode === '--native' ? first : second;
  const output = mode === '--native' ? second : first;
  if (mode === '--native' && !output.endsWith('.settings.json')) throw new Error('Native output must end in .settings.json');
  if ([source, source.slice(0, -5) + '.richschema.json'].some(file => path.resolve(file) === path.resolve(output))) {
    throw new Error('Output must not overwrite metadata sources');
  }
  const metadata = packageMetadata(source);
  atomicWrite(output, mode === '--native' ? metadata : embedMetadata(readBounded(output, WASM_LIMIT), metadata));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(`Theme metadata: ${error.message}`); process.exitCode = 1; }
}
