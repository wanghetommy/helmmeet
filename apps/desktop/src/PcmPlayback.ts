/** Gapless PCM playback. Segments share one AudioContext timeline, never a new media element. */
export interface PcmChunk { samples: number[]; start_ms: number; end_ms: number }
export interface PlaybackProgress { position: number; playing: boolean }
export class PcmPlayback {
  private context: AudioContext | undefined;
  private gain: GainNode | undefined;
  private epoch = 0;
  private nodes = new Set<AudioBufferSourceNode>();
  private timer: ReturnType<typeof setInterval> | undefined;
  private cancelWait: (() => void) | undefined;
  private origin = 0;
  private startMs = 0;
  private endMs = 0;
  private running = false;
  private volume = 1;
  private readonly fetch: (meeting: string, start: number, variant: string, end: number) => Promise<PcmChunk>;
  private readonly progress: (v: PlaybackProgress) => void;
  private readonly error: (e: string) => void;
  private readonly createContext: () => AudioContext;
  constructor(fetch: (meeting: string, start: number, variant: string, end: number) => Promise<PcmChunk>, progress: (v: PlaybackProgress) => void, error: (e: string) => void, createContext: () => AudioContext = () => new AudioContext()) {
    this.fetch = fetch; this.progress = progress; this.error = error; this.createContext = createContext;
  }
  private currentPosition() { return Math.min(this.endMs, Math.max(this.startMs, this.startMs + ((this.context?.currentTime ?? 0) - this.origin) * 1000)); }
  setVolume(volume: number) { this.volume = Math.min(1, Math.max(0, volume)); if (this.context && this.gain) { this.gain.gain.cancelScheduledValues(this.context.currentTime); this.gain.gain.setTargetAtTime(this.volume, this.context.currentTime, .005); } }
  stop(report = true) {
    const position = this.currentPosition();
    this.epoch++; this.running = false; this.cancelWait?.();
    if (this.timer) clearInterval(this.timer); this.timer = undefined;
    const now = this.context?.currentTime ?? 0;
    if (this.gain) { this.gain.gain.cancelScheduledValues(now); this.gain.gain.setTargetAtTime(0, now, .002); }
    for (const node of this.nodes) { try { node.stop(now + .015); } catch { /* already ended */ } }
    this.nodes.clear();
    if (report) this.progress({ position, playing: false });
  }
  async close() { this.stop(false); const c = this.context; this.context = undefined; this.gain = undefined; if (c) await c.close(); }
  async play(meeting: string, start: number, duration: number, variant = 'raw') {
    this.stop(false); const epoch = this.epoch;
    this.startMs = start; this.endMs = duration;
    try {
      const context = this.context ??= this.createContext();
      // Resume while still in the user's click gesture, before awaiting IPC.
      await context.resume();
      if (!this.gain) { this.gain = context.createGain(); this.gain.connect(context.destination); }
      const gain = this.gain;
      const first = await this.fetch(meeting, Math.floor(start), variant, duration);
      if (epoch !== this.epoch) return;
      this.origin = context.currentTime + .05; this.running = true;
      gain.gain.cancelScheduledValues(context.currentTime); gain.gain.setValueAtTime(0, this.origin); gain.gain.linearRampToValueAtTime(this.volume, this.origin + .005);
      this.timer = setInterval(() => { if (epoch === this.epoch) this.progress({ position: this.currentPosition(), playing: this.running }); }, 100);
      this.progress({ position: start, playing: true });
      let next: { chunk: PcmChunk; at: number } | undefined = { chunk: first, at: this.origin };
      while (next && epoch === this.epoch) next = await this.schedule(meeting, next.chunk, next.at, epoch, variant, duration);
    } catch (e) { if (epoch === this.epoch) { this.stop(); this.error(String(e)); } }
  }
  private async schedule(meeting: string, chunk: PcmChunk, at: number, epoch: number, variant: string, duration: number): Promise<{ chunk: PcmChunk; at: number } | undefined> {
    if (epoch !== this.epoch || !this.context || !this.gain) return;
    const context = this.context;
    if (context.currentTime > at - .01) throw new Error('回听数据未及时准备，已暂停；可从当前位置重试');
    if (!chunk.samples.length || chunk.end_ms <= chunk.start_ms || chunk.samples.length !== (chunk.end_ms - chunk.start_ms) * 16) throw new Error('回听音频长度无效');
    const buffer = context.createBuffer(1, chunk.samples.length, 16000);
    buffer.copyToChannel(Float32Array.from(chunk.samples, s => Math.max(-32768, Math.min(32767, s)) / 32768), 0);
    const node = context.createBufferSource(); node.buffer = buffer; this.nodes.add(node);
    const nextAt = at + chunk.samples.length / 16000;
    const final = chunk.end_ms >= this.endMs;
    const envelope = final ? context.createGain() : undefined;
    if (envelope) { const fade = Math.min(.005, buffer.duration / 2); envelope.gain.setValueAtTime(1, at); envelope.gain.setValueAtTime(1, nextAt - fade); envelope.gain.linearRampToValueAtTime(0, nextAt); node.connect(envelope); envelope.connect(this.gain); }
    else node.connect(this.gain);
    node.onended = () => { this.nodes.delete(node); node.disconnect(); envelope?.disconnect(); if (epoch === this.epoch && final) { this.running = false; if (this.timer) clearInterval(this.timer); this.timer = undefined; this.progress({ position: this.endMs, playing: false }); } };
    node.start(at);
    if (final) return;
    // One chunk of prefetch; do not load a whole long recording into memory.
    const next = await this.fetch(meeting, chunk.end_ms, variant, duration);
    if (epoch !== this.epoch) return;
    if (next.start_ms !== chunk.end_ms) throw new Error('回听分段不连续');
    if (context.currentTime > nextAt - .01) throw new Error('回听数据未及时准备，已暂停；可从当前位置重试');
    const waitMs = Math.max(0, (at - context.currentTime) * 1000);
    // Schedule the prefetched chunk when this one starts. Stop/seek cancels the timer.
    await new Promise<void>(resolve => {
      const finish = () => { clearTimeout(timer); if (this.cancelWait === finish) this.cancelWait = undefined; resolve(); };
      const timer = setTimeout(finish, waitMs);
      this.cancelWait = finish;
      if (epoch !== this.epoch) finish();
    });
    if (epoch !== this.epoch) return;
    return { chunk: next, at: nextAt };
  }
}
