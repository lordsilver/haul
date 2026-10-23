import { useSyncExternalStore } from 'react';
import type { ArchiveView } from '@/lib/bindings/ArchiveView';
import type { ItemView } from '@/lib/bindings/ItemView';
import type { State } from '@/lib/bindings/State';
import { prefs } from '@/lib/prefs';
import type { ListKey } from '@/lib/route';

// One row of the list: a download, or an archive in parts with the parts
// this view shows (`members`, in part order). The archive's row sums up the
// whole archive in every view, as the engine sums it (`state.archives`).
export type Entry = { it: ItemView } | { key: string; archive: ArchiveView; members: ItemView[] };

export const inList = (it: ItemView, view: ListKey) => view === 'all' || it.list === view;

export function matchesQuery(it: ItemView, query: string): boolean {
  if (!query) return true;
  return [it.name, it.package, it.host, it.source_name, ...it.mirrors].some((v) => v?.toLowerCase().includes(query));
}

// The list in display order, each archive at the place of its first part
// shown.
export function entriesOf(visible: ItemView[], s: State): Entry[] {
  const archives = new Map(s.archives.map((a) => [a.key, a]));
  const out: Entry[] = [];
  const placed = new Map<string, { key: string; archive: ArchiveView; members: ItemView[] }>();
  for (const it of visible) {
    const archive = it.archive ? archives.get(it.archive) : undefined;
    if (!archive) {
      out.push({ it });
      continue;
    }
    let entry = placed.get(archive.key);
    if (!entry) {
      entry = { key: archive.key, archive, members: [] };
      placed.set(archive.key, entry);
      out.push(entry);
    }
    entry.members.push(it);
  }
  for (const entry of placed.values()) {
    const order = new Map(entry.archive.ids.map((id, i) => [id, i]));
    entry.members.sort((a, b) => (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0));
  }
  return out;
}

// The ids in display order, an archive's parts together even while folded.
export const orderOf = (entries: Entry[]) =>
  entries.flatMap((e) => ('members' in e ? e.members.map((i) => i.id) : [e.it.id]));

// Archives the user opened, in this browser only; folded is the default.
let expanded = new Set<string>(
  (() => {
    try {
      return JSON.parse(prefs.get('expanded') ?? '[]') as string[];
    } catch {
      return [];
    }
  })(),
);
const listeners = new Set<() => void>();

export function toggleArchive(key: string) {
  const next = new Set(expanded);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expanded = next;
  prefs.set('expanded', JSON.stringify([...next].slice(-200)));
  for (const listener of listeners) listener();
}

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useExpanded = () => useSyncExternalStore(subscribe, () => expanded);
