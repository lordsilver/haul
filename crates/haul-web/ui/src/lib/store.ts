import type { State } from '@/lib/bindings/State';
import { paths } from '@/lib/client';
import { externalStore } from '@/lib/external-store';
import { type ToastKind, toast } from '@/lib/toast';

// What the engine has to tell the person (`notice` events).
type Notice = { title: string; body: string; kind: 'finished' | 'failed' | 'notice' };

const TONE: Record<Notice['kind'], ToastKind> = { finished: 'success', failed: 'error', notice: 'info' };

// The rows that did not change keep the object they had, so a row that
// renders only from its own object is skipped (`memo`).
function shared<T extends { id: string }>(before: T[] | undefined, now: T[]): T[] {
  if (!before) return now;
  const old = new Map(before.map((x) => [x.id, x]));
  return now.map((x) => {
    const was = old.get(x.id);
    return was && JSON.stringify(was) === JSON.stringify(x) ? was : x;
  });
}

// The engine's state as it streams in over /api/events, and how the stream
// stands: lost for a moment (`offline`), or answered by another version of
// Haul than the one that served this page (`updated`).
export type Live = { state: State | null; offline: boolean; updated: boolean };

let live: Live = { state: null, offline: false, updated: false };
const store = externalStore(() => live);
let pageVersion: string | null = null;

function set(next: Partial<Live>) {
  live = { ...live, ...next };
  store.notify();
}

export function connect() {
  const events = new EventSource(paths.events);
  let timer: ReturnType<typeof setTimeout> | null = null;
  events.addEventListener('state', (e) => {
    if (timer) clearTimeout(timer);
    timer = null;
    const state: State = JSON.parse((e as MessageEvent<string>).data);
    state.items = shared(live.state?.items, state.items);
    // Another version means Haul was updated and restarted under this page,
    // whose code the new one may no longer understand.
    pageVersion ??= state.version;
    set({ state, offline: false, updated: state.version !== pageVersion });
  });
  events.addEventListener('notice', (e) => {
    const n: Notice = JSON.parse((e as MessageEvent<string>).data);
    // The desktop app tells it itself while this window is in the background.
    if (document.hasFocus()) toast(n.body ? `${n.title}: ${n.body}` : n.title, TONE[n.kind]);
  });
  events.addEventListener('error', () => {
    timer ??= setTimeout(() => set({ offline: true }), 1500);
    // The browser reconnects by itself, except after an answer that is no
    // event stream (an error page while Haul starts up, say): start over.
    if (events.readyState === EventSource.CLOSED) setTimeout(connect, 3000);
  });
}

export const useLive = store.use;

// The latest state, for event handlers that must not wait for a render.
export const currentState = () => live.state;
