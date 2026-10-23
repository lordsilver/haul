import { useSyncExternalStore } from 'react';

// The lists of downloads, each item knowing its own (`list`) and the engine
// counting them; the wrappers; and the pages.
export const LISTS = ['all', 'active', 'paused', 'done', 'attention', 'failed'] as const;
export const PAGES = ['wrappers', 'hosts', 'connections', 'settings'] as const;
export type ListKey = (typeof LISTS)[number];
export type ViewKey = ListKey | (typeof PAGES)[number];

export const isList = (view: ViewKey): view is ListKey => (LISTS as readonly string[]).includes(view);

function fromHash(): ViewKey {
  const name = location.hash.replace(/^#\/?/, '');
  return ([...LISTS, ...PAGES] as readonly string[]).includes(name) ? (name as ViewKey) : 'all';
}

export const go = (view: ViewKey) => {
  location.hash = `#/${view}`;
};

const subscribe = (listener: () => void) => {
  addEventListener('hashchange', listener);
  return () => removeEventListener('hashchange', listener);
};

export const useView = () => useSyncExternalStore(subscribe, fromHash);

// #/add/<links> opens the Add dialog with the links filled in (a bookmarklet
// can send the page it is on); adding still takes the user's click. Answers
// the links, once, and puts the list back in the address.
export function takeAddRoute(): string | null {
  const match = location.hash.match(/^#\/add(?:\/(.*))?$/s);
  if (!match) return null;
  history.replaceState(null, '', '#/all');
  try {
    return decodeURIComponent(match[1] ?? '');
  } catch {
    return '';
  }
}
