import assert from 'node:assert/strict';
import test from 'node:test';
import { PcmPlayback, type PcmChunk } from '../../apps/desktop/src/PcmPlayback.ts';
function fixture() {
  const nodes: { at: number; duration: number; stopped: boolean }[] = [];
  const parameter = { cancelScheduledValues() {}, setTargetAtTime() {}, setValueAtTime() {}, linearRampToValueAtTime() {} };
  const context = {
    currentTime: 10, destination: {}, async resume() {}, async close() {},
    createGain: () => ({ gain: parameter, connect() {}, disconnect() {} }),
    createBuffer: (_: number, length: number, rate: number) => ({ duration: length / rate, copyToChannel() {} }),
    createBufferSource: () => {
      const node = { buffer: { duration: 0 }, onended: null, connect() {}, disconnect() {},
        start(at: number) { nodes.push({ at, duration: this.buffer.duration, stopped: false }); }, stop() { nodes.forEach(n => { n.stopped = true; }); } };
      return node;
    },
  };
  return { context, nodes, create: () => context as unknown as AudioContext };
}
const chunk = (start: number, end: number): PcmChunk => ({ samples: new Array((end - start) * 16).fill(100), start_ms: start, end_ms: end });
test('consecutive 30-second chunks use exactly adjacent AudioContext times', async () => {
  const f = fixture(); const requested: number[] = []; const errors: string[] = [];
  const player = new PcmPlayback(async (_, start) => { requested.push(start); return chunk(start, start + 30000); }, () => {}, e => errors.push(e), f.create);
  try {
    await player.play('fixture', 0, 60000);
    assert.deepEqual(requested, [0, 30000]); assert.equal(f.nodes.length, 2);
    assert.equal(f.nodes[1]!.at, f.nodes[0]!.at + f.nodes[0]!.duration);
    assert.deepEqual(errors, []);
  } finally { await player.close(); }
});
test('a late fetch from an old meeting cannot start audio or replace the new playback', async () => {
  const f = fixture(); let release: (c: PcmChunk) => void = () => {};
  const old = new Promise<PcmChunk>(resolve => { release = resolve; });
  const player = new PcmPlayback(async meeting => meeting === 'old' ? old : chunk(0, 1000), () => {}, e => { throw new Error(e); }, f.create);
  try {
    const first = player.play('old', 0, 1000); await Promise.resolve();
    await player.play('new', 0, 1000); release(chunk(0, 1000)); await first;
    assert.equal(f.nodes.length, 1);
  } finally { await player.close(); }
});
test('an underrun stops explicitly instead of inserting a hidden playback gap', async () => {
  const f = fixture(); const errors: string[] = [];
  const player = new PcmPlayback(async (_, start) => { if (start) f.context.currentTime = 45; return chunk(start, start + 30000); }, () => {}, e => errors.push(e), f.create);
  try {
    await player.play('fixture', 0, 60000);
    assert.equal(f.nodes.length, 1); assert.ok(f.nodes[0]!.stopped);
    assert.ok(errors[0]?.includes('未及时准备'));
  } finally { await player.close(); }
});
test('changing audio variant fences late raw response and freezes enhanced prefetch limit', async () => {
  const f = fixture(); let release: (c: PcmChunk) => void = () => {};
  const raw = new Promise<PcmChunk>(resolve => { release = resolve; });
  const calls: { variant: string; end: number }[] = [];
  const player = new PcmPlayback(async (_, start, variant, end) => { calls.push({variant,end}); return variant === 'raw' ? raw : chunk(start, Math.min(start + 30000, end)); }, () => {}, e => { throw new Error(e); }, f.create);
  try {
    const first = player.play('same-meeting', 0, 60000, 'raw'); await Promise.resolve();
    await player.play('same-meeting', 0, 31000, 'enhanced'); release(chunk(0,30000)); await first;
    assert.equal(f.nodes.length,2); assert.equal(f.nodes[1]!.duration,1);
    assert.deepEqual(calls, [{variant:'raw',end:60000},{variant:'enhanced',end:31000},{variant:'enhanced',end:31000}]);
  } finally {await player.close();}
});
