import { useSyncExternalStore } from 'react';

// Rows picked in the current view, Windows style: alone with a plain click
// (once picking has started), added or dropped with Ctrl, a range with
// Shift. `order` is the view's rows in display order, for ranges.
let picked = new Set<string>();
let anchor: string | null = null;
const listeners = new Set<() => void>();

function publish(next: Set<string>) {
  picked = next;
  for (const listener of listeners) listener();
}

export function pickMany(ids: string[], order: string[], { toggle = false, range = false } = {}) {
  const next = new Set(picked);
  if (range && anchor && order.includes(anchor)) {
    const spots = [anchor, ...ids].map((id) => order.indexOf(id)).filter((i) => i >= 0);
    if (!toggle) next.clear();
    for (const id of order.slice(Math.min(...spots), Math.max(...spots) + 1)) next.add(id);
  } else if (toggle) {
    const all = ids.every((id) => next.has(id));
    for (const id of ids) {
      if (all) next.delete(id);
      else next.add(id);
    }
    anchor = ids.at(-1) ?? null;
  } else {
    next.clear();
    for (const id of ids) next.add(id);
    anchor = ids.at(-1) ?? null;
  }
  publish(next);
}

export function selectAll(ids: string[]) {
  publish(new Set([...picked, ...ids]));
}

export function clearSelection() {
  if (!picked.size) return;
  anchor = null;
  publish(new Set());
}

// Rows that left the view (removed, finished elsewhere, filtered out) drop
// out of the selection; folded parts of an archive stay in it.
export function keepOnly(ids: Set<string>) {
  if ([...picked].every((id) => ids.has(id))) return;
  publish(new Set([...picked].filter((id) => ids.has(id))));
}

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useSelection = () => useSyncExternalStore(subscribe, () => picked);
export const currentSelection = () => picked;

// Whether a click on a row picks it. A selection starts at a checkbox (or
// with Ctrl or Shift), so a stray click on a row picks nothing; once one has
// started, a click anywhere on a row adds or drops it, as in Gmail.
export function picking(e: React.MouseEvent) {
  const target = e.target as HTMLElement;
  return Boolean(target.closest('[data-check]')) || e.ctrlKey || e.metaKey || e.shiftKey || picked.size > 0;
}
