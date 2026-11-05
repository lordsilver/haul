import type { State } from '@/lib/bindings/State';
import { externalStore } from '@/lib/external-store';

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
  const events = new EventSource('/api/events');
  let timer: ReturnType<typeof setTimeout> | null = null;
  events.addEventListener('state', (e) => {
    if (timer) clearTimeout(timer);
    timer = null;
    const state: State = JSON.parse((e as MessageEvent<string>).data);
    // Another version means Haul was updated and restarted under this page,
    // whose code the new one may no longer understand.
    pageVersion ??= state.version;
    set({ state, offline: false, updated: state.version !== pageVersion });
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
