import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { desktop, getLaunchRequest, type NewMeetingLaunch } from './bridge';

interface Options {
  ready: boolean; canOpen: boolean; autoOpen: boolean;
  open: (request: NewMeetingLaunch) => Promise<boolean>;
  fail: (message: string) => void;
}
export function useLaunchRequests(options: Options) {
  const current = useRef(options); current.current = options;
  const [request, setRequest] = useState<NewMeetingLaunch | null>(null);
  const [busy, setBusy] = useState(false);
  const opening = useRef(false);
  const live = useRef(false);
  const queue = useRef<Promise<void>>(Promise.resolve());
  // Reads do not consume cold-start events. Acknowledge only after navigation or
  // explicit dismissal, and serialize reads so a stale poll cannot undo an ack.
  const read = (acknowledge: string | null = null) => {
    const next = queue.current.then(async () => {
      const value = await getLaunchRequest(acknowledge);
      if (live.current) setRequest(value);
    });
    queue.current = next.catch(e => { if (live.current) current.current.fail(String(e)); });
    return next;
  };
  useEffect(() => {
    if (!desktop) return;
    live.current = true;
    let cancelled = false;
    const stops: (() => void)[] = [];
    const register = async () => {
      for (const [name, fn] of [
        ['launch-link', () => { void read().catch(() => {}); }],
        ['launch-link-error', (event: { payload: unknown }) => { if (typeof event.payload === 'string') current.current.fail(event.payload); }],
      ] as const) {
        const stop = await listen(name, fn);
        if (cancelled) { stop(); return; }
        stops.push(stop);
      }
      await read();
    };
    void register().catch(e => { if (!cancelled) current.current.fail(String(e)); });
    return () => { cancelled = true; live.current = false; stops.forEach(stop => stop()); };
  }, []);
  const open = async () => {
    if (!request || opening.current || !current.current.ready || !current.current.canOpen) return;
    opening.current = true; setBusy(true);
    try {
      if (await current.current.open(request)) await read(request.id);
    } catch (e) { current.current.fail(String(e)); }
    finally { opening.current = false; if (live.current) setBusy(false); }
  };
  const dismiss = async () => {
    if (!request || opening.current) return;
    opening.current = true; setBusy(true);
    try { await read(request.id); }
    catch { /* The shared reader reports the error. */ }
    finally { opening.current = false; if (live.current) setBusy(false); }
  };
  useEffect(() => {
    if (request && options.ready && options.canOpen && options.autoOpen) void open();
  }, [request, options.ready, options.canOpen, options.autoOpen]);
  return { request, busy, open, dismiss };
}
