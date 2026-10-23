import {
  ArrowDown,
  ArrowUp,
  Folder,
  Gauge,
  GripVertical,
  Info,
  KeyRound,
  Minus,
  Plus,
  Server,
  Shield,
  Star,
} from 'lucide-react';
import { type FormEvent, type ReactNode, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { IconButton } from '@/components/icon-button';
import { Infobar } from '@/components/infobar';
import { Button } from '@/components/ui/button';
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { api, report } from '@/lib/api';
import type { HostView } from '@/lib/bindings/HostView';
import type { State } from '@/lib/bindings/State';
import { bytes, duration, ordinal } from '@/lib/format';
import { cn } from '@/lib/utils';
import { showAccount } from '@/views/settings';

const host = (name: string, path: string) => `/api/host/${encodeURIComponent(name)}/${path}`;

const setRoute = (name: string, route: 'proxy' | 'direct') => api(host(name, `route/${route}`));

// The preferred hosts in the order downloads try them, as the engine sorts
// the hosts.
const preferredOrder = (hosts: HostView[]) => hosts.filter((h) => h.group === 'preferred').map((h) => h.name);

const setPreferredOrder = (hosts: string[]) => api('/api/preferred-hosts', { hosts }).catch(report);

const HEADINGS: Record<HostView['group'], [string, string]> = {
  preferred: ['Preferred, in order', 'A file downloads from the first of these that has it.'],
  other: ['Other hosts', 'Used when no preferred host has the file, the fastest first.'],
  off: ['Off', 'Their links are left alone: switched off, or waiting for an account.'],
};

function Pill({
  tone = 'plain',
  title,
  children,
}: {
  tone?: 'plain' | 'caution' | 'success' | 'star';
  title?: string;
  children: ReactNode;
}) {
  return (
    <span
      title={title}
      className={cn(
        'inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs [&_svg]:size-3',
        tone === 'plain' && 'text-muted-foreground',
        tone === 'caution' && 'border-warn-line bg-warn-soft text-warn-strong',
        tone === 'success' && 'border-transparent bg-ok-soft text-ok-strong',
        tone === 'star' && 'border-transparent bg-info-soft text-info-strong',
      )}
    >
      {children}
    </span>
  );
}

// What it has done and what a free user gets there, where Haul knows.
function stats(h: HostView): string {
  const out = [
    h.speed ? `${bytes(h.speed)}/s per file` : 'Speed not measured yet',
    `${h.ok} done`,
    `${h.fails} failed`,
    `${bytes(h.bytes)} total`,
  ];
  if (h.cooldown) out.push(`cooling down ${duration(h.cooldown)}`);
  if (h.account) out.push('account set');
  else if (h.free) {
    const free = [];
    if (h.free.at_once) free.push(`${h.free.at_once} at a time`);
    if (h.free.daily_bytes)
      free.push(
        `${bytes(h.free.daily_bytes)} a day at full speed${h.free.slowed_to ? `, then ${bytes(h.free.slowed_to)}/s` : ''}`,
      );
    if (free.length) out.push(`free: ${free.join(', ')}`);
  }
  // Its own word on what an address may fetch at full speed today.
  if (h.quota) out.push(`${bytes(h.quota.used)} used today`);
  return out.join(' · ');
}

function Pills({ h, proxy }: { h: HostView; proxy: boolean }) {
  const off = h.group === 'off';
  return (
    <>
      {h.preferred !== null ? (
        <Pill tone="star">
          <Star className="fill-current" />
          {ordinal(h.preferred + 1)} choice
        </Pill>
      ) : null}
      {h.disabled ? (
        <Pill tone="caution">Never used</Pill>
      ) : h.needs_account ? (
        <Pill tone="caution" title="It gives no file without an account, so its links are left out until you set one">
          <KeyRound />
          Needs an account
        </Pill>
      ) : null}
      {h.exits > 1 ? (
        <Pill title="Downloads spread over these addresses; its limits count for each">
          <Shield />
          {h.exits} addresses
        </Pill>
      ) : null}
      {h.quota?.slowed ? (
        <Pill
          tone={h.quota.slowed === h.quota.known ? 'caution' : 'plain'}
          title="Past the bytes it allows an address at full speed today; it serves the rest slowly"
        >
          <Gauge />
          {h.quota.slowed === h.quota.known
            ? `Slowed to ${bytes(h.quota.slowed_to)}/s`
            : `${h.quota.known - h.quota.slowed} of ${h.quota.known} addresses at full speed`}
        </Pill>
      ) : null}
      {h.fastest && !off ? <Pill tone="success">Fastest</Pill> : null}
      {h.folder ? (
        <Pill title="Its files go to this subfolder">
          <Folder />
          {h.folder}
        </Pill>
      ) : null}
      {h.cap !== null && h.cap < h.limit ? (
        <Pill title="It refused one more download while these ran, so Haul runs no more than this, and tries one more after ten quiet minutes. The slot buttons set your own limit.">
          <Info />
          Takes {h.cap} at a time
        </Pill>
      ) : null}
      {proxy ? (
        <Pill tone={h.route === 'direct' ? 'caution' : 'plain'}>
          <Shield />
          via {h.route}
        </Pill>
      ) : null}
    </>
  );
}

// The subfolder for one host's files, typed in a form under its card.
function FolderForm({ h, dir, close }: { h: HostView; dir: string; close: () => void }) {
  const [folder, setFolder] = useState(h.folder);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api(host(h.name, 'folder'), { folder: folder.trim() });
      close();
    } catch (error) {
      report(error);
    }
  };
  return (
    <form className="flex flex-col gap-1.5 pl-9" onSubmit={submit}>
      <div className="flex flex-wrap items-center gap-2">
        <Input
          autoFocus
          value={folder}
          placeholder="Subfolder"
          aria-label={`Folder for files from ${h.name}`}
          className="max-w-xs"
          onChange={(e) => setFolder(e.target.value)}
          onKeyDown={(e) => e.key === 'Escape' && close()}
        />
        <Button type="submit">Save</Button>
        <Button type="button" variant="ghost" onClick={close}>
          Cancel
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Files from {h.name} go to this folder inside {dir}. Leave it empty for the download folder itself.
      </p>
    </form>
  );
}

function RouteBar({ h, proxy }: { h: HostView; proxy: boolean }) {
  if (h.blocked && h.route === 'proxy') {
    return (
      <Infobar
        kind="critical"
        title="Looks blocked through the proxy."
        actions={
          <ConfirmButton
            variant="outline"
            size="sm"
            label="Allow direct"
            confirm="Use my real IP address?"
            action={() => setRoute(h.name, 'direct')}
          />
        }
      >
        The host answered 403/451 through it.
      </Infobar>
    );
  }
  if (proxy && h.route === 'direct') {
    return (
      <Infobar
        kind="caution"
        title="Direct."
        actions={
          <Button size="sm" variant="outline" onClick={() => setRoute(h.name, 'proxy').catch(report)}>
            Back to the proxy
          </Button>
        }
      >
        This host sees your real IP address.
      </Infobar>
    );
  }
  return null;
}

type Drop = { name: string; after: boolean } | null;

// Dragging a preferred host by its grip puts it elsewhere in the order. The
// drag carries a type of its own, so the window's drop zone for links and
// files leaves it alone.
const HOST_DRAG = 'application/x-haul-host';

function HostCard({
  h,
  s,
  place,
  count,
  drop,
  dragging,
  onDragStart,
  onDragEnd,
  move,
}: {
  h: HostView;
  s: State;
  place: number;
  count: number;
  drop: Drop;
  dragging: boolean;
  onDragStart: () => void;
  onDragEnd: () => void;
  move: (delta: number) => void;
}) {
  const [showSlots, setShowSlots] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draggable, setDraggable] = useState(false);
  const off = h.group === 'off';
  const preferred = h.preferred !== null;
  // The slot buttons show when a limit is set or learned, or asked for.
  const stepper = (h.limited || showSlots) && !off;
  const setLimit = (delta: number) => api(host(h.name, `limit/${Math.max(1, h.slots + delta)}`)).catch(report);
  const starTitle = preferred ? 'Preferred: click to stop preferring it' : 'Prefer this host';
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: a pointer drag from the grip; the arrows reorder from the keyboard
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
        {/* The host's icon, or the grip that drags it when it can move;
            draggable only from there, so the card's text stays selectable. */}
        {h.movable ? (
          <span
            title="Drag to change the order"
            className="cursor-grab text-muted-foreground"
            onPointerDown={() => setDraggable(true)}
          >
            <GripVertical aria-hidden className="size-5" />
          </span>
        ) : (
          <Server aria-hidden className="size-5 shrink-0 text-muted-foreground" />
        )}
        <div className="min-w-0 flex-[1_1_280px]">
          <div className="text-sm font-medium">{h.name}</div>
          <div className="text-sm text-muted-foreground">{stats(h)}</div>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Pills h={h} proxy={s.proxy} />
        </div>
        <div className="flex flex-wrap items-center justify-end gap-1">
          {stepper ? (
            <span className="flex items-center gap-1" title="Files downloaded from this host at the same time">
              <IconButton label="Fewer slots" disabled={h.slots <= 1} onClick={() => setLimit(-1)}>
                <Minus />
              </IconButton>
              <output className="min-w-20 text-center text-xs tabular-nums">
                {h.active} / {h.slots} {h.slots === 1 ? 'slot' : 'slots'}
                {h.exits > 1 ? ` × ${h.exits}` : ''}
              </output>
              <IconButton label="More slots" disabled={h.slots >= s.slots_max} onClick={() => setLimit(1)}>
                <Plus />
              </IconButton>
            </span>
          ) : null}
          {h.needs_account && !h.disabled ? (
            <Button size="sm" onClick={() => showAccount(h.name)}>
              <KeyRound />
              Set account
            </Button>
          ) : null}
          {!stepper && !off ? (
            <IconButton label="Limit downloads at once" onClick={() => setShowSlots(true)}>
              <Gauge />
            </IconButton>
          ) : null}
          {/* Two preferred hosts or more can trade places. */}
          {h.movable ? (
            <>
              <IconButton
                label="Move up in the preferred order"
                data-move="up"
                disabled={place === 0}
                onClick={() => move(-1)}
              >
                <ArrowUp />
              </IconButton>
              <IconButton
                label="Move down in the preferred order"
                data-move="down"
                disabled={place === count - 1}
                onClick={() => move(1)}
              >
                <ArrowDown />
              </IconButton>
            </>
          ) : null}
          <IconButton
            label={starTitle}
            aria-pressed={preferred}
            onClick={() => api(host(h.name, 'preferred'), { preferred: !preferred }).catch(report)}
          >
            <Star className={cn(preferred && 'fill-current text-info')} />
          </IconButton>
          <IconButton label="Folder for this host's files" onClick={() => setEditing(true)}>
            <Folder />
          </IconButton>
          {/* Waiting for an account, it is off whatever the switch says. */}
          {h.needs_account && !h.disabled ? null : (
            <span
              className="ml-1 flex items-center gap-2 text-sm text-muted-foreground"
              title={
                h.disabled
                  ? 'Off: its links are never tried. Click to use it again.'
                  : 'On: click to never use this host'
              }
            >
              {h.disabled ? 'Off' : 'On'}
              <Switch
                checked={!h.disabled}
                aria-label={h.disabled ? `Use ${h.name}` : `Never use ${h.name}`}
                onCheckedChange={(on) => api(host(h.name, 'disabled'), { disabled: !on }).catch(report)}
              />
            </span>
          )}
        </div>
      </div>
      <RouteBar h={h} proxy={s.proxy} />
      {editing ? <FolderForm h={h} dir={s.download_dir} close={() => setEditing(false)} /> : null}
    </div>
  );
}

export function Hosts({ s }: { s: State }) {
  // A drag keeps the cards still until it ends: the order as it began.
  const [drag, setDrag] = useState<{ name: string; frozen: HostView[] } | null>(null);
  const [drop, setDrop] = useState<Drop>(null);
  const list = useRef<HTMLDivElement>(null);
  const hosts = drag?.frozen ?? s.hosts;
  const order = preferredOrder(hosts);
  // Moving a card takes the focus off its arrow; it goes back once the card
  // has moved, or to the other arrow once that one ran out.
  const refocus = useRef<{ name: string; dir: 'up' | 'down' } | null>(null);
  useLayoutEffect(() => {
    const want = refocus.current;
    if (!want || !list.current) return;
    const card = list.current.querySelector(`[data-host="${CSS.escape(want.name)}"]`);
    const same = card?.querySelector<HTMLButtonElement>(`[data-move=${want.dir}]`);
    const target = same && !same.disabled ? same : card?.querySelector<HTMLButtonElement>('[data-move]:not(:disabled)');
    if (target) {
      target.focus({ preventScroll: true });
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
    refocus.current = { name, dir: delta < 0 ? 'up' : 'down' };
    setPreferredOrder(next);
  };

  const headings = new Set<string>();
  return (
    <>
      <header className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Hosts</h1>
        <p className="max-w-3xl text-sm text-muted-foreground">
          Choose which hosts Haul uses and in what order. A file on several hosts downloads from the first of your
          preferred hosts that has it (star a host to add it; drag it by its grip, or use its arrows, to reorder),
          otherwise from the fastest of the others. A host switched off is never used. The gauge limits how many files
          download from a host at once, and the folder keeps its files in a subfolder.
        </p>
      </header>
      {hosts.length ? (
        // biome-ignore lint/a11y/noStaticElementInteractions: where a dragged host lands; the arrows reorder from the keyboard
        <div
          ref={list}
          className="-mx-2 min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-2 pb-4"
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
          <div className="flex max-w-4xl flex-col gap-2.5">
            {hosts.map((h) => {
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
                  place={order.indexOf(h.name)}
                  count={order.length}
                  drop={drop}
                  dragging={drag?.name === h.name}
                  onDragStart={() => setDrag({ name: h.name, frozen: s.hosts })}
                  onDragEnd={() => setDrag(null)}
                  move={(delta) => move(h.name, delta)}
                />,
              ];
            })}
          </div>
        </div>
      ) : (
        <Empty className="flex-1 border">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <Server />
            </EmptyMedia>
            <EmptyTitle>No host contacted yet</EmptyTitle>
            <EmptyDescription>Hosts appear here once Haul talks to them.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </>
  );
}
