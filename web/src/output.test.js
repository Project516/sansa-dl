import { test } from 'node:test';
import { strict as assert } from 'node:assert';
import { explain, safeName, validateMp3 } from './output.js';

test('names fit on the card and reject invalid characters', () => {
  assert.equal(safeName('a/b?.webm'), 'a_b_.mp3');
  assert.ok(new TextEncoder().encode(safeName('é'.repeat(200) + '.webm')).length <= 247);
});

test('explains an unreachable proxy', () => {
  assert.match(explain(new Error("TransportError('TypeError: Failed to fetch')")), /reach your proxy/);
  assert.equal(explain('boom'), 'Could not make the MP3: boom');
});

test('checks the tag size and MP3 header', () => {
  assert.doesNotThrow(() => validateMp3(Uint8Array.from([0xff, 0xfb, 0x90, 0x00])));
  assert.throws(() => validateMp3(Uint8Array.from([0x49,0x44,0x33,3,0,0,1,0,0,0])));
  assert.throws(() => validateMp3(Uint8Array.from([1,2,3,4])));
});
