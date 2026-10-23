import { useEffect, useRef } from 'react';
import { openHere } from '@/lib/api';
import type { State } from '@/lib/bindings/State';
import { isWebLink, plural } from '@/lib/format';
import { type ViewKey, go } from '@/lib/route';
import { currentState } from '@/lib/store';
import { type ToastAction, toast } from '@/lib/toast';

type Waiting = { id: string; name: string; open_urls: string[]; wrapper: boolean };

// A wrapper waits for a person like a download does, with its own name and
// its own section to land on.
function waitingNow(s: State): Waiting[] {
  return [
    ...s.items.filter((it) => it.status === 'captcha').map((it) => ({ ...it, wrapper: false })),
    ...s.wrappers
      .filter((w) => w.status === 'captcha')
      .map((w) => ({ id: w.id, name: w.title || w.url, open_urls: w.open_urls, wrapper: true })),
  ];
}

function needsToast(items: Waiting[], desktop: boolean) {
  const one = items.length === 1 ? items[0] : null;
  const place: ViewKey = items.every((it) => it.wrapper) ? 'wrappers' : 'attention';
  let action: ToastAction | null = null;
  if (one && desktop) {
    action = { label: 'Open here', onClick: () => openHere(one.id) };
  } else if (one) {
    const url = one.open_urls.find(isWebLink);
    // Opens the page in a tab of its own; Haul lands on the list that waits,
    // ready for the link the page hands back.
    if (url) action = { label: 'Open page', href: url, onClick: () => go(place) };
  }
  action ??= { label: 'Show', onClick: () => go(place) };
  const what = place === 'wrappers' ? 'wrappers' : 'downloads';
  toast(one ? `${one.name} needs you` : `${items.length} ${what} need you`, 'info', action);
}

// Calls for attention: downloads that entered the "Needs you" state,
// gathered for a moment so a burst shares one toast, and wrappers just
// unwrapped, whose files wait for a start. The first state after opening
// Haul raises nothing: those were already parked where they are.
export function useAttention(s: State | null, view: ViewKey) {
  const seen = useRef<Map<string, string> | null>(null);
  const needs = useRef(new Map<string, Waiting>());
  const needsTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
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
    for (const it of waitingNow(s)) {
      if (before.get(it.id) !== 'captcha') needs.current.set(it.id, it);
    }
    if (needs.current.size && !needsTimer.current) {
      needsTimer.current = setTimeout(() => {
        needsTimer.current = null;
        // One resolved in the moment since (a pasted link, a try) is not
        // announced after the fact.
        const now = currentState();
        const still = new Set(now ? waitingNow(now).map((it) => it.id) : []);
        const items = [...needs.current.values()].filter((it) => still.has(it.id));
        needs.current.clear();
        if (items.length) needsToast(items, Boolean(now?.desktop));
      }, 1200);
    }
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
