import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { parseSystemStatus } from '../../packages/contracts/src/index.ts';

test('desktop status fixture can cross the IPC boundary', () => {
  const fixture = JSON.parse(readFileSync(new URL('../fixtures/system-status.json', import.meta.url), 'utf8'));
  assert.deepEqual(parseSystemStatus(fixture), fixture);
});

test('rejects protocol drift, missing fields and string booleans', () => {
  const valid = { protocol_version: 1, runtime: 'tauri', capture_available: false,
    asr_available: false, analysis_available: false };
  for (const invalid of [null, {}, { ...valid, protocol_version: 2 },
    { ...valid, runtime: 'unknown' }, { ...valid, capture_available: 'false' },
    { ...valid, asr_available: undefined }, { ...valid, analysis_available: 1 }]) {
    assert.throws(() => parseSystemStatus(invalid));
  }
});

test('does not propagate unexpected backend fields into the UI', () => {
  const value = parseSystemStatus({ protocol_version: 1, runtime: 'tauri',
    capture_available: false, asr_available: false, analysis_available: false,
    credential_ref: 'must-not-propagate' });
  assert.equal('credential_ref' in value, false);
});
