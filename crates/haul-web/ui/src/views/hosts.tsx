import {
  ClipboardList,
  Clock,
  Folder,
  Gauge,
  Globe,
  GripVertical,
  Info,
  KeyRound,
  Link2,
  Server,
  Shield,
  Star,
  Wrench,
} from 'lucide-react';
import { type FormEvent, type ReactNode, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { IconButton } from '@/components/icon-button';
import { Infobar } from '@/components/infobar';
import { EmptyState, PageHeader, PageScroll, SearchBox } from '@/components/page';
import { AccountField, SettingCard, SettingRow } from '@/components/setting-card';
import { SettingsTabs } from '@/components/settings-tabs';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { api, report } from '@/lib/api';
import type { AccountForm } from '@/lib/bindings/AccountForm';
import type { HostView } from '@/lib/bindings/HostView';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { bytes, duration, ordinal, plural } from '@/lib/format';
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
  other: ['Other hosts', 'Used when no preferred host has the file, the fastest first; one switched off never.'],
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
  tone?: 'plain' | 'caution' | 'star';
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
        tone === 'star' && 'border-transparent bg-info-soft text-info-strong',
      )}
    >
      {children}
    </span>
  );
}

// What a free user gets there, in one form for every host: an account set
// replaces it, and a host with nothing known says so.
function freeAccess(h: HostView): string {
  if (h.account) return 'Account set';
  const free: string[] = [];
  if (h.free?.daily_bytes) {
    const after = h.free.slowed_to ? `, then ${bytes(h.free.slowed_to)}/s` : '';
    free.push(`${bytes(h.free.daily_bytes)} a day${after}`);
  }
  if (h.free?.at_once) free.push(`${h.free.at_once} at a time`);
  if (h.free?.per_file) free.push(h.free.per_file);
  return free.join(' · ') || 'No limits known';
}

// What has come from it so far, always in this order.
function yourDownloads(h: HostView): string {
  const used: string[] = [];
  if (h.bytes) used.push(`${bytes(h.bytes)}`);
  if (h.ok) used.push(plural(h.ok, 'file'));
  if (h.speed) used.push(`${bytes(h.speed)}/s`);
  if (h.fails) used.push(`${h.fails} failed`);
  if (h.quota) used.push(`${bytes(h.quota.used)} today`);
  return used.join(' · ') || 'Not used yet';
}

// A wrapper's or a paste's own: pages opened, as they hold no files.
function yourLinks(h: HostView): string {
  const used: string[] = [];
  if (h.ok) used.push(`Opened ${plural(h.ok, 'time')}`);
  if (h.fails) used.push(`${h.fails} failed`);
  return used.join(' · ') || 'Not used yet';
}

function Pills({ h, proxy }: { h: HostView; proxy: boolean }) {
  return (
    <>
      {h.cooldown ? (
        <Pill tone="caution" title="It asked this address to wait">
          <Clock />
          Waiting {duration(h.cooldown)}
        </Pill>
      ) : null}
      {h.kind === 'host' && h.preferred !== null ? (
        <Pill tone="star">
          <Star className="fill-current" />
          {ordinal(h.preferred + 1)} choice
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
      {h.cap !== null ? (
        <Pill title="It refused one more download while these ran, so Haul runs no more than this, and tries one more after ten quiet minutes.">
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
function AccountPanel({ form, d }: { form: AccountForm; d: SettingsView }) {
  const env = envNote(d, ...form.fields.map((f) => f.key));
  return (
    <div className="flex flex-col gap-2">
      {env || form.note ? <p className="text-xs text-muted-foreground">{env ?? form.note}</p> : null}
      <div className="flex flex-wrap items-center gap-2">
        {form.fields.map((f) => (
          <AccountField key={f.key} d={d} name={f.key} label={f.label} secret={f.secret} />
        ))}
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

// Whether Haul uses this site.
function HostSwitch({ h }: { h: HostView }) {
  const off = h.disabled;
  return (
    <span
      className="ml-1 flex items-center gap-2 text-sm text-muted-foreground"
      title={
        h.disabled
          ? 'Off: its links are never tried. Click to use it again.'
          : h.needs_account
            ? 'On, used only when no other host has the file: free, it needs you for each one. Set its account under the wrench to use it like any other.'
            : 'On: click to never use this host'
      }
    >
      {off ? 'Off' : 'On'}
      <Switch
        checked={!off}
        aria-label={off ? `Use ${h.name}` : `Never use ${h.name}`}
        onCheckedChange={(on) => api(host(h.name, 'disabled'), { disabled: !on }).catch(report)}
      />
    </span>
  );
}

// A site's own settings, opened from its card: for a host, its account and
// its folder; for any site, the way it is reached when there is a proxy.
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
  return (
    <>
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

// Wrappers and pastebins are looked up now and then, unlike the file hosts the
// page is for: each kind opens from a button rather than fill the list.
const LINK_SITES = ['wrapper', 'paste'] as const;
const isLinkSite = (h: HostView) => (LINK_SITES as readonly string[]).includes(h.group);

function LinkSite({ h, s, d }: { h: HostView; s: State; d: SettingsView | null }) {
  const [open, setOpen] = useState(false);
  const Icon = KIND_ICONS[h.kind];
  const account = s.accounts.find((a) => a.host === h.name);
  return (
    <div
      className={cn(
        'flex flex-col gap-3 rounded-lg border bg-card px-3 py-2.5',
        h.disabled && 'opacity-70',
        // Its settings need the room.
        (open || h.blocked) && 'sm:col-span-2',
      )}
    >
      <div className="flex min-h-7 flex-wrap items-center gap-x-2.5 gap-y-1.5">
        <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1 text-sm">
          <div className="truncate font-medium" title={h.name}>
            {h.name}
          </div>
          <div className="truncate text-muted-foreground" title={freeAccess(h)}>
            {freeAccess(h)}
          </div>
          <div className={cn('truncate text-muted-foreground tabular-nums', !h.ok && !h.fails && 'opacity-60')}>
            {yourLinks(h)}
          </div>
        </div>
        <Pills h={h} proxy={s.proxy} />
        {s.proxy ? (
          <IconButton label={`Settings for ${h.name}`} aria-expanded={open} onClick={() => setOpen(!open)}>
            <Wrench className={cn(open && 'text-foreground')} />
          </IconButton>
        ) : null}
        {h.disabled ? <HostSwitch h={h} /> : null}
      </div>
      <RouteBar h={h} proxy={s.proxy} />
      {open ? <HostSettings h={h} s={s} d={d} account={account} /> : null}
    </div>
  );
}

function LinkSitesButton({
  group,
  sites,
  s,
  d,
}: {
  group: (typeof LINK_SITES)[number];
  sites: HostView[];
  s: State;
  d: SettingsView | null;
}) {
  const Icon = KIND_ICONS[group];
  const [title, description] = HEADINGS[group];
  return (
    <Dialog>
      <DialogTrigger asChild>
        <Button variant="outline">
          <Icon aria-hidden />
          {title}
          <span className="text-muted-foreground tabular-nums">{sites.length}</span>
        </Button>
      </DialogTrigger>
      <DialogContent
        className="sm:max-w-2xl"
        // On the dialog itself, or its first button's tooltip opens with it.
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          (e.currentTarget as HTMLElement).focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <div className="-mx-1 grid max-h-[60vh] gap-2 overflow-y-auto px-1 sm:grid-cols-2">
          {sites.map((h) => (
            <LinkSite key={h.name} h={h} s={s} d={d} />
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}

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
