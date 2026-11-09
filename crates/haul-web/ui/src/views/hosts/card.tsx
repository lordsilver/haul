// One host as a card, with its pills, its switch and its settings.
import { GripVertical, Star, Wrench } from 'lucide-react';
import { useState } from 'react';
import { Hint } from '@/components/hint';
import { IconButton } from '@/components/icon-button';
import { report } from '@/lib/api';
import type { HostView } from '@/lib/bindings/HostView';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { hosts as hostRoutes } from '@/lib/client';
import { cn } from '@/lib/utils';
import { KIND_ICONS, Pills, freeAccess, yourDownloads } from '@/views/hosts/pills';
import { type Drop, HOST_DRAG } from '@/views/hosts/reorder';
import { HostSettings, HostSwitch, RouteBar } from '@/views/hosts/settings';

export function HostCard({
  h,
  s,
  d,
  drop,
  dragging,
  onDragStart,
  onDragEnd,
  move,
}: {
  h: HostView;
  s: State;
  d: SettingsView | null;
  drop: Drop;
  dragging: boolean;
  onDragStart: () => void;
  onDragEnd: () => void;
  move: (delta: number) => void;
}) {
  const [open, setOpen] = useState(false);
  const [draggable, setDraggable] = useState(false);
  const account = s.accounts.find((a) => a.host === h.name);
  // Files download from a host; the others only lead to links, so they have
  // no order, account or folder.
  const fileHost = h.kind === 'host';
  const off = h.disabled;
  const Icon = KIND_ICONS[h.kind];
  const preferred = h.preferred !== null;
  // Its settings: a host's account and folder, and any site's route when
  // there is a proxy to go around.
  const settings = fileHost || s.proxy;
  const starTitle = preferred ? 'Preferred: click to stop preferring it' : 'Prefer this host';
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: a pointer drag from the grip, which takes the arrow keys
    <div
      data-host={h.name}
      draggable={draggable}
      onPointerUp={() => setDraggable(false)}
      onDragStart={(e) => {
        if (e.target !== e.currentTarget) return;
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData(HOST_DRAG, h.name);
        onDragStart();
      }}
      onDragEnd={(e) => {
        if (e.target !== e.currentTarget) return;
        setDraggable(false);
        onDragEnd();
      }}
      className={cn(
        'relative flex flex-col gap-3 rounded-xl border bg-card px-4 py-3.5',
        off && 'opacity-70',
        dragging && 'opacity-40',
        drop?.name === h.name && (drop.after ? 'after:-bottom-1.5' : 'after:-top-1.5'),
        drop?.name === h.name && 'after:absolute after:inset-x-2 after:h-0.5 after:rounded-full after:bg-info',
      )}
    >
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        {/* The host's icon, or the grip that moves it when it can: dragged,
            or with the arrow keys once focused. Draggable only from there,
            so the card's text stays selectable. */}
        {h.movable ? (
          <Hint text="Drag, or press ↑ ↓, to change the order">
            <button
              type="button"
              data-grip
              aria-label={`Move ${h.name} in the preferred order with the arrow keys`}
              className="-m-1 cursor-grab rounded-md p-1 text-muted-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
              onPointerDown={() => setDraggable(true)}
              onKeyDown={(e) => {
                if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
                e.preventDefault();
                move(e.key === 'ArrowUp' ? -1 : 1);
              }}
            >
              <GripVertical aria-hidden className="size-5" />
            </button>
          </Hint>
        ) : (
          <Icon aria-hidden className="size-5 shrink-0 text-muted-foreground" />
        )}
        <div className="min-w-0 flex-[1_1_280px] text-sm">
          <div className="truncate font-medium" title={h.name}>
            {h.name}
          </div>
          {/* The same two things on every host, lined up down the list. */}
          {fileHost ? (
            <div className="gap-x-4 text-muted-foreground min-[1180px]:grid min-[1180px]:grid-cols-[17rem_minmax(0,1fr)]">
              <div className="truncate" title={freeAccess(h)}>
                {freeAccess(h)}
              </div>
              <div
                className={cn('truncate tabular-nums', !h.bytes && !h.ok && !h.fails && 'opacity-60')}
                title={yourDownloads(h)}
              >
                {yourDownloads(h)}
              </div>
            </div>
          ) : null}
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Pills h={h} proxy={s.proxy} />
        </div>
        <div className="flex flex-wrap items-center justify-end gap-1">
          {fileHost ? (
            <IconButton
              label={starTitle}
              aria-pressed={preferred}
              onClick={() => hostRoutes.preferred(h.name, !preferred).catch(report)}
            >
              <Star className={cn(preferred && 'fill-current text-info')} />
            </IconButton>
          ) : null}
          {settings ? (
            <IconButton label={`Settings for ${h.name}`} aria-expanded={open} onClick={() => setOpen(!open)}>
              <Wrench className={cn(open && 'text-foreground')} />
            </IconButton>
          ) : null}
          {/* Wrappers and pastebins are listed to look up, not to turn off;
              one switched off before keeps its switch, to turn it back on. */}
          {fileHost || h.disabled ? <HostSwitch h={h} /> : null}
        </div>
      </div>
      <RouteBar h={h} proxy={s.proxy} />
      {open ? <HostSettings h={h} s={s} d={d} account={account} /> : null}
    </div>
  );
}
