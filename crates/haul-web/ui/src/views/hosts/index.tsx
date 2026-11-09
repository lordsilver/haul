// Settings › Hosts: every site Haul knows or met, grouped and in order.
import { Server } from 'lucide-react';
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { EmptyState, PageHeader, PageScroll, SearchBox } from '@/components/page';
import { SettingsTabs } from '@/components/settings-tabs';
import type { HostView } from '@/lib/bindings/HostView';
import type { State } from '@/lib/bindings/State';
import { useSettings } from '@/lib/settings';
import { HostCard } from '@/views/hosts/card';
import { LINK_SITES, LinkSitesButton, isLinkSite } from '@/views/hosts/link-sites';
import { HEADINGS } from '@/views/hosts/pills';
import { type Drop, HOST_DRAG, preferredOrder, setPreferredOrder } from '@/views/hosts/reorder';
import { OtherAccounts } from '@/views/hosts/settings';

export function Hosts({ s }: { s: State }) {
  const d = useSettings(s.config_version);
  const [typed, setTyped] = useState('');
  // A drag keeps the cards still until it ends: the order as it began.
  const [drag, setDrag] = useState<{ name: string; frozen: HostView[] } | null>(null);
  const [drop, setDrop] = useState<Drop>(null);
  const list = useRef<HTMLDivElement>(null);
  // The sites the engine lists: not the pages and hosts Haul has no code
  // for, which are used without a setting.
  const hosts = (drag?.frozen ?? s.hosts).filter((h) => h.listed);
  const order = preferredOrder(hosts);
  // A card moved with the arrow keys keeps the focus on its grip once it
  // shows in its new place.
  const refocus = useRef<string | null>(null);
  useLayoutEffect(() => {
    const want = refocus.current;
    if (!want || !list.current) return;
    const grip = list.current.querySelector<HTMLButtonElement>(`[data-host="${CSS.escape(want)}"] [data-grip]`);
    if (grip) {
      grip.focus({ preventScroll: true });
      refocus.current = null;
    }
  });
  useEffect(() => {
    if (!drag) setDrop(null);
  }, [drag]);

  const move = (name: string, delta: number) => {
    const next = preferredOrder(s.hosts);
    const from = next.indexOf(name);
    const to = from + delta;
    if (from < 0 || to < 0 || to >= next.length) return;
    next.splice(to, 0, ...next.splice(from, 1));
    refocus.current = name;
    setPreferredOrder(next);
  };

  const headings = new Set<string>();
  const words = typed.trim().toLowerCase();
  const listed = hosts.filter((h) => !isLinkSite(h) && (!words || h.name.includes(words)));
  const linkSites = LINK_SITES.map((group) => ({ group, sites: hosts.filter((h) => h.group === group) })).filter(
    (kind) => kind.sites.length,
  );
  return (
    <>
      <PageHeader title="Settings" tabs={<SettingsTabs current="hosts" />} />
      <div className="flex max-w-4xl flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          {linkSites.map(({ group, sites }) => (
            <LinkSitesButton key={group} group={group} sites={sites} s={s} d={d} />
          ))}
        </div>
        <SearchBox value={typed} label="Search hosts" placeholder="Search hosts" onChange={setTyped} />
      </div>
      {hosts.length ? (
        // Where a dragged host lands; the arrows reorder from the keyboard.
        <PageScroll
          ref={list}
          onDragOver={(e) => {
            if (!drag || !e.dataTransfer.types.includes(HOST_DRAG)) return;
            const node = (e.target as HTMLElement).closest<HTMLElement>('[data-host]');
            const target = node ? hosts.find((h) => h.name === node.dataset.host) : undefined;
            if (!node || !target?.movable || target.name === drag.name) {
              setDrop(null);
              return;
            }
            e.preventDefault();
            e.dataTransfer.dropEffect = 'move';
            const box = node.getBoundingClientRect();
            const after = e.clientY > box.top + box.height / 2;
            if (drop?.name !== target.name || drop.after !== after) setDrop({ name: target.name, after });
          }}
          onDragLeave={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node)) setDrop(null);
          }}
          onDrop={(e) => {
            if (!drag || !drop) return;
            e.preventDefault();
            e.stopPropagation();
            const next = order.filter((name) => name !== drag.name);
            next.splice(next.indexOf(drop.name) + (drop.after ? 1 : 0), 0, drag.name);
            setDrop(null);
            setPreferredOrder(next);
          }}
        >
          {listed.map((h) => {
            // A heading where the next group starts: preferred, the rest, off.
            const heading = headings.has(h.group) ? null : HEADINGS[h.group];
            headings.add(h.group);
            return [
              heading ? (
                <div key={`heading-${h.group}`} className="mt-3 flex flex-wrap items-baseline gap-x-2 first:mt-0">
                  <strong className="text-sm">{heading[0]}</strong>
                  <span className="text-sm text-muted-foreground">{heading[1]}</span>
                </div>
              ) : null,
              <HostCard
                key={h.name}
                h={h}
                s={s}
                d={d}
                drop={drop}
                dragging={drag?.name === h.name}
                onDragStart={() => setDrag({ name: h.name, frozen: s.hosts })}
                onDragEnd={() => setDrag(null)}
                move={(delta) => move(h.name, delta)}
              />,
            ];
          })}
          {listed.length ? null : <p className="text-sm text-muted-foreground">No host matches.</p>}
          <OtherAccounts s={s} d={d} />
        </PageScroll>
      ) : (
        <EmptyState
          icon={Server}
          title="No sites"
          description="The hosts, wrappers and pastebins Haul knows appear here."
        >
          <div className="flex flex-col items-center gap-2.5">
            <OtherAccounts s={s} d={d} />
          </div>
        </EmptyState>
      )}
    </>
  );
}
