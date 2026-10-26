import { useSyncExternalStore } from 'react';
import type { ItemView } from '@/lib/bindings/ItemView';

// The lists of downloads, each item knowing its own (`list`) and the engine
// counting them; the wrappers; and the pages.
export const LISTS = ['all', 'active', 'paused', 'done', 'attention', 'failed'] as const;
export const PAGES = ['wrappers', 'hosts', 'connections', 'settings'] as const;
export type ListKey = (typeof LISTS)[number];
export type ViewKey = ListKey | (typeof PAGES)[number];

// The lists are `all` and exactly the engine's (`ItemView['list']`): one
// added on either side fails to compile here rather than land on All.
type Same<A, B> = [Exclude<A, B> | Exclude<B, A>] extends [never] ? true : false;
true satisfies Same<ListKey, ItemView['list'] | 'all'>;

const VIEWS: readonly string[] = [...LISTS, ...PAGES];

export const isList = (view: ViewKey): view is ListKey => (LISTS as readonly string[]).includes(view);

const named = () => location.hash.replace(/^#\/?/, '');

function fromHash(): ViewKey {
  const name = named();
  return VIEWS.includes(name) ? (name as ViewKey) : 'all';
}

export const go = (view: ViewKey) => {
  location.hash = `#/${view}`;
};

// Puts `view` in the address without a history entry. A replaced address
// fires no hashchange of its own, so one is sent for `useView` to follow.
function replace(view: ViewKey) {
  history.replaceState(null, '', `#/${view}`);
  dispatchEvent(new HashChangeEvent('hashchange'));
}

const subscribe = (listener: () => void) => {
  addEventListener('hashchange', listener);
  return () => removeEventListener('hashchange', listener);
};

export const useView = () => useSyncExternalStore(subscribe, fromHash);

// Settles the address, at start and on each change: #/add/<links> opens the
// Add dialog with the links filled in (a bookmarklet can send the page it is
// on, the desktop app what it was asked to open); adding still takes the
// user's click. Answers those links, once, and puts the list back in the
// address, as it does for an address that names no view.
export function followRoute(): string | null {
  const match = location.hash.match(/^#\/add(?:\/(.*))?$/s);
  if (!match) {
    if (!VIEWS.includes(named())) replace('all');
    return null;
  }
  replace('all');
  return decoded(match[1] ?? '');
}

// `text` percent-decoded. A script's own link may not encode a `%` of its
// links (`100%-done`), which fails decoding as a whole: each character's
// escapes (one to four, as its first byte says) are decoded on their own
// then, and those that are no character kept as they came.
function decoded(text: string): string {
  try {
    return decodeURIComponent(text);
  } catch {
    return text.replace(/%[0-9a-f]{2}(?:%[89ab][0-9a-f]){0,3}/gi, (escapes) => {
      try {
        return decodeURIComponent(escapes);
      } catch {
        return escapes;
      }
    });
  }
}
