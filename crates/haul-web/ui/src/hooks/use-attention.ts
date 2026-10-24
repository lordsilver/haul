import { useEffect, useRef } from 'react';
import type { State } from '@/lib/bindings/State';
import { plural } from '@/lib/format';
import { type ViewKey, go } from '@/lib/route';
import { currentState } from '@/lib/store';
import { toast } from '@/lib/toast';

// Calls for attention: wrappers just unwrapped, whose files wait for a
// start. (What needs the user shows in the status bar, and the desktop app
// notifies when a page wants them.) The first state after opening Haul
// raises nothing: those were already parked where they are.
export function useAttention(s: State | null, view: ViewKey) {
  const seen = useRef<Map<string, string> | null>(null);
  const unwrapped = useRef(new Set<string>());
  const unwrappedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The wrappers the unwrapped toast tells of while it shows: those that
  // finish later join it rather than raising a toast of their own.
  const told = useRef(new Set<string>());
  const viewRef = useRef(view);
  viewRef.current = view;

  useEffect(() => {
    if (!s) return;
    const statuses = new Map([...s.items, ...s.wrappers].map((it) => [it.id, it.status as string]));
    const before = seen.current;
    seen.current = statuses;
    if (!before) return;
    for (const w of s.wrappers) {
      const was = before.get(w.id);
      if (w.status === 'done' && was && was !== 'done') unwrapped.current.add(w.id);
    }
    if (unwrapped.current.size && !unwrappedTimer.current) {
      unwrappedTimer.current = setTimeout(() => {
        unwrappedTimer.current = null;
        for (const id of unwrapped.current) told.current.add(id);
        unwrapped.current.clear();
        const now = currentState();
        // Already where the toast would lead.
        if (!now || viewRef.current === 'wrappers') {
          told.current.clear();
          return;
        }
        const done = now.wrappers.filter((w) => told.current.has(w.id));
        if (!done.length) return;
        // Their files counted once (wrappers of one release share them),
        // named by the release when there is one, and a button for the list
        // rather than starting what may be several releases at once.
        const ids = new Set(done.map((w) => w.id));
        const cards = now.wrapper_cards.filter((c) => c.ids.some((id) => ids.has(id)));
        const waiting = cards.reduce((n, c) => n + c.tally.not_started + c.tally.paused, 0);
        const names = [...new Set(done.map((w) => w.title || w.url))];
        const what = names.length === 1 ? names[0] : plural(names.length, 'release');
        toast(
          waiting ? `${what}: ${plural(waiting, 'file')} ready to start` : `${what} unwrapped`,
          'info',
          { label: 'Show', onClick: () => go('wrappers') },
          'unwrapped',
          // Once it is gone, the next wrapper to finish starts a toast of its own.
          () => told.current.clear(),
        );
      }, 2000);
    }
  }, [s]);
}
