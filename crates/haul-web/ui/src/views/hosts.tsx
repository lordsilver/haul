import {
  ClipboardList,
  Folder,
  Gauge,
  Globe,
  GripVertical,
  Info,
  KeyRound,
  Link2,
  Minus,
  Plus,
  Server,
  Shield,
  Star,
  Wrench,
} from 'lucide-react';
import { type FormEvent, type ReactNode, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { IconButton } from '@/components/icon-button';
import { Infobar } from '@/components/infobar';
import { AccountField, SettingCard, SettingRow } from '@/components/setting-card';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { api, report } from '@/lib/api';
import type { AccountForm } from '@/lib/bindings/AccountForm';
import type { HostView } from '@/lib/bindings/HostView';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { bytes, duration, ordinal } from '@/lib/format';
import { envNote, useSettings } from '@/lib/settings';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';

const host = (name: string, path: string) => `/api/host/${encodeURIComponent(name)}/${path}`;

const setRoute = (name: string, route: 'proxy' | 'direct') => api(host(name, `route/${route}`));

// The preferred hosts in the order downloads try them, as the engine sorts
// the hosts.
const preferredOrder = (hosts: HostView[]) => hosts.filter((h) => h.group === 'preferred').map((h) => h.name);

const setPreferredOrder = (hosts: string[]) => api('/api/preferred-hosts', { hosts }).catch(report);

const HEADINGS: Record<HostView['group'], [string, string]> = {
  preferred: ['Preferred hosts, in order', 'A file downloads from the first of these that has it.'],
  other: ['Hosts', 'Used when no preferred host has the file, the fastest first; one switched off never.'],
  wrapper: ['Wrappers', 'Sites that hold download links on file hosts (containers, link protectors).'],
  paste: ['Pastebins', 'Paste sites whose pages list links.'],
  site: ['Other sites', 'Sites Haul has no code for that you gave a setting; the rest are used without one.'],
};

// Each kind's icon; a host that can move shows its grip instead.
const KIND_ICONS: Record<HostView['kind'], typeof Server> = {
  host: Server,
  wrapper: Link2,
  paste: ClipboardList,
  site: Globe,
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
  // Only links are read from a wrapper, a paste or a site: no speed, files
  // or allowance to tell.
  if (h.kind !== 'host') return h.cooldown ? `Waiting ${duration(h.cooldown)}` : '';
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
  return (
    <>
      {h.kind === 'host' && h.preferred !== null ? (
        <Pill tone="star">
          <Star className="fill-current" />
          {ordinal(h.preferred + 1)} choice
        </Pill>
      ) : null}
      {h.needs_account && !h.disabled ? (
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

// The subfolder for one host's files.
function FolderForm({ h, dir }: { h: HostView; dir: string }) {
  const [folder, setFolder] = useState(h.folder);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const chosen = folder.trim();
    try {
      await api(host(h.name, 'folder'), { folder: chosen });
      toast(
        chosen ? `Files from ${h.name} go to ${chosen}` : `Files from ${h.name} go to the download folder`,
        'success',
      );
    } catch (error) {
      report(error);
    }
  };
  return (
    <form className="flex flex-col gap-1.5" onSubmit={submit}>
      <div className="flex flex-wrap items-center gap-2">
        <Input
          value={folder}
          placeholder="Subfolder"
          aria-label={`Folder for files from ${h.name}`}
          className="max-w-xs"
          onChange={(e) => setFolder(e.target.value)}
        />
        <Button type="submit" variant="outline" disabled={folder.trim() === h.folder}>
          Save
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Files from {h.name} go to this folder inside {dir}. Leave it empty for the download folder itself.
      </p>
    </form>
  );
}

// A host's account, typed in its card: each field saved as it is left.
function AccountPanel({ form, d, close }: { form: AccountForm; d: SettingsView; close?: () => void }) {
  const env = envNote(d, ...form.fields.map((f) => f.key));
  return (
    <div className="flex flex-col gap-2">
      {env || form.note ? <p className="text-xs text-muted-foreground">{env ?? form.note}</p> : null}
      <div className="flex flex-wrap items-center gap-2">
        {form.fields.map((f) => (
          <AccountField key={f.key} d={d} name={f.key} label={f.label} secret={f.secret} />
        ))}
        {close ? (
          <Button variant="ghost" onClick={close}>
            Done
          </Button>
        ) : null}
      </div>
    </div>
  );
}

// Accounts for hosts the list does not show yet (no links of theirs so
// far): those set, and a menu to add another.
function OtherAccounts({ s, d }: { s: State; d: SettingsView | null }) {
  const [adding, setAdding] = useState<string[]>([]);
  const extras = s.accounts.filter((a) => !s.hosts.some((h) => h.name === a.host));
  if (!d || !extras.length) return null;
  const shown = extras.filter((a) => a.set || adding.includes(a.host));
  const more = extras.filter((a) => !shown.includes(a));
  return (
    <>
      <div className="mt-3 flex flex-wrap items-baseline gap-x-2">
        <strong className="text-sm">Accounts</strong>
        <span className="text-sm text-muted-foreground">For hosts with no links here yet.</span>
      </div>
      {shown.map((a) => (
        <SettingCard key={a.host} icon={KeyRound} title={a.host}>
          <AccountPanel form={a} d={d} />
        </SettingCard>
      ))}
      {more.length ? (
        <div>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline">
                <KeyRound />
                Add an account
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start">
              {more.map((a) => (
                <DropdownMenuItem key={a.host} onSelect={() => setAdding([...adding, a.host])}>
                  {a.host}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      ) : null}
    </>
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
  // no slots, order, account or folder.
  const fileHost = h.kind === 'host';
  const off = h.disabled || h.needs_account;
  const Icon = KIND_ICONS[h.kind];
  const preferred = h.preferred !== null;
  // Its settings: a host's slots, account and folder, and any site's route
  // when there is a proxy to go around.
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
          <button
            type="button"
            data-grip
            title="Drag, or press ↑ ↓, to change the order"
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
        ) : (
          <Icon aria-hidden className="size-5 shrink-0 text-muted-foreground" />
        )}
        <div className="min-w-0 flex-[1_1_280px]">
          <div className="text-sm font-medium">{h.name}</div>
          <div className="text-sm text-muted-foreground">{stats(h)}</div>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Pills h={h} proxy={s.proxy} />
        </div>
        <div className="flex flex-wrap items-center justify-end gap-1">
          {h.needs_account && !h.disabled ? (
            <Button size="sm" aria-expanded={open} onClick={() => setOpen(!open)}>
              <KeyRound />
              Set account
            </Button>
          ) : null}
          {fileHost ? (
            <IconButton
              label={starTitle}
              aria-pressed={preferred}
              onClick={() => api(host(h.name, 'preferred'), { preferred: !preferred }).catch(report)}
            >
              <Star className={cn(preferred && 'fill-current text-info')} />
            </IconButton>
          ) : null}
          {settings ? (
            <IconButton label={`Settings for ${h.name}`} aria-expanded={open} onClick={() => setOpen(!open)}>
              <Wrench className={cn(open && 'text-foreground')} />
            </IconButton>
          ) : null}
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
      {open ? <HostSettings h={h} s={s} d={d} account={account} /> : null}
    </div>
  );
}

// A site's own settings, opened from its card: for a host, the files it
// downloads at once, its account and its folder; for any site, the way it
// is reached when there is a proxy.
function HostSettings({
  h,
  s,
  d,
  account,
}: {
  h: HostView;
  s: State;
  d: SettingsView | null;
  account: AccountForm | undefined;
}) {
  const fileHost = h.kind === 'host';
  const setLimit = (delta: number) => api(host(h.name, `limit/${Math.max(1, h.slots + delta)}`)).catch(report);
  return (
    <>
      {fileHost ? (
        <SettingRow
          title="Downloads at once"
          description={
            h.cap
              ? `It refused more than ${h.cap} at a time, so Haul keeps to that.`
              : 'Files downloaded from this host at the same time.'
          }
          controls={
            <span className="flex items-center gap-1">
              <IconButton label="Fewer at once" disabled={h.slots <= 1} onClick={() => setLimit(-1)}>
                <Minus />
              </IconButton>
              <output className="min-w-20 text-center text-sm tabular-nums">
                {h.active} / {h.slots}
                {h.exits > 1 ? ` × ${h.exits}` : ''}
              </output>
              <IconButton label="More at once" disabled={h.slots >= s.slots_max} onClick={() => setLimit(1)}>
                <Plus />
              </IconButton>
            </span>
          }
        />
      ) : null}
      {fileHost && account && d ? (
        <SettingRow title="Account" description={account.set ? 'Set.' : 'Optional: it may lift the free limits.'}>
          <AccountPanel form={account} d={d} />
        </SettingRow>
      ) : null}
      {fileHost ? (
        <SettingRow title="Folder">
          <FolderForm h={h} dir={s.download_dir} />
        </SettingRow>
      ) : null}
      {s.proxy ? (
        <SettingRow
          title="Route"
          description={
            h.route === 'direct'
              ? 'Direct: this site sees your real IP address.'
              : 'Through the proxies, as every site goes unless you allow it direct.'
          }
          controls={
            h.route === 'direct' ? (
              <Button variant="outline" onClick={() => setRoute(h.name, 'proxy').catch(report)}>
                Back to the proxy
              </Button>
            ) : (
              <ConfirmButton
                variant="outline"
                label="Allow direct"
                confirm="Use my real IP address?"
                action={() => setRoute(h.name, 'direct')}
              />
            )
          }
        />
      ) : null}
    </>
  );
}

export function Hosts({ s }: { s: State }) {
  const d = useSettings(s.config_version);
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
  return (
    <>
      <header className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Sites</h1>
        <p className="max-w-3xl text-sm text-muted-foreground">
          The file hosts your downloads are on, and the wrappers and pastebins that lead to them. Choose which hosts
          Haul uses and in what order. A file on several hosts downloads from the first of your preferred hosts that has
          it (star a host to add it; drag it by its grip to reorder), otherwise from the fastest of the others. A host
          switched off is never used. The wrench opens a site's settings: how many files download from it at once, its
          account, its folder and its route.
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
                  d={d}
                  drop={drop}
                  dragging={drag?.name === h.name}
                  onDragStart={() => setDrag({ name: h.name, frozen: s.hosts })}
                  onDragEnd={() => setDrag(null)}
                  move={(delta) => move(h.name, delta)}
                />,
              ];
            })}
            <OtherAccounts s={s} d={d} />
          </div>
        </div>
      ) : (
        <Empty className="flex-1 border">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <Server />
            </EmptyMedia>
            <EmptyTitle>No sites</EmptyTitle>
            <EmptyDescription>The hosts, wrappers and pastebins Haul knows appear here.</EmptyDescription>
          </EmptyHeader>
          <div className="flex flex-col items-center gap-2.5">
            <OtherAccounts s={s} d={d} />
          </div>
        </Empty>
      )}
    </>
  );
}
