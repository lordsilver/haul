import { ArrowDown, ChevronRight, ClipboardList, Gauge, LoaderCircle, Play, Shield, TriangleAlert } from 'lucide-react';
import { type ReactNode, useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { Switch } from '@/components/ui/switch';
import { UpdateEntry } from '@/components/update';
import { fetchSettings, report, save } from '@/lib/api';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { bytes, plural, roughly } from '@/lib/format';
import { go } from '@/lib/route';
import { envNote } from '@/lib/settings';
import { THEMES, setTheme, useTheme } from '@/lib/theme';
import { cn } from '@/lib/utils';

const item = 'inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-xs';

// A small panel over the status bar that changes a setting in place, filled
// from the settings as they are when it opens.
function Flyout({
  title,
  trigger,
  on = false,
  action,
  children,
}: {
  title: string;
  trigger: ReactNode;
  on?: boolean;
  // Beside the title, such as the way to the page that sets this up.
  action?: (close: () => void) => ReactNode;
  children: (d: SettingsView, close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [settings, setSettings] = useState<SettingsView | null>(null);
  useEffect(() => {
    if (!open) return;
    let current = true;
    fetchSettings().then(
      (d) => current && setSettings(d),
      (error) => {
        report(error);
        setOpen(false);
      },
    );
    return () => {
      current = false;
    };
  }, [open]);
  return (
    <Popover open={open && settings !== null} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          title={title}
          onClick={() => setOpen(!open)}
          className={cn(item, 'text-muted-foreground hover:bg-accent hover:text-foreground', on && 'text-foreground')}
        >
          {trigger}
        </button>
      </PopoverTrigger>
      <PopoverContent side="top" align="start" className="w-80" aria-label={title}>
        <div className="mb-2 flex min-h-8 items-center justify-between gap-2 font-medium">
          {title}
          {action?.(() => setOpen(false))}
        </div>
        {settings ? children(settings, () => setOpen(false)) : null}
      </PopoverContent>
    </Popover>
  );
}

// A switch that flips at once and saves; `change` answers whether it saved.
function FlyoutSwitch({
  label,
  on,
  change,
  disabled = false,
}: {
  label: string;
  on: boolean;
  change: (on: boolean) => Promise<boolean>;
  disabled?: boolean;
}) {
  const [value, setValue] = useState(on);
  useEffect(() => setValue(on), [on]);
  return (
    <div className="flex items-center justify-between gap-3 py-1.5 text-sm">
      <span>{label}</span>
      <Switch
        checked={value}
        disabled={disabled}
        aria-label={label}
        onCheckedChange={async (next) => {
          setValue(next);
          if (!(await change(next))) setValue(!next);
        }}
      />
    </div>
  );
}

function SpeedLimit({ d, close }: { d: SettingsView; close: () => void }) {
  const fixed = envNote(d, 'rate_limit_mbps');
  const [text, setText] = useState(d.rate_limit_mbps ? String(d.rate_limit_mbps) : '');
  const apply = async (mbps: number) => {
    if (await save({ rate_limit_mbps: mbps }, null)) close();
  };
  const set = () => {
    const n = text.trim() === '' ? 0 : Number(text);
    if (Number.isFinite(n) && n >= 0) apply(n);
  };
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-muted-foreground">{fixed ?? 'For all downloads together.'}</p>
      <form
        className="flex items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          set();
        }}
      >
        <Input
          type="number"
          min={0}
          step={0.5}
          value={text}
          placeholder="None"
          aria-label="Speed limit in MB/s"
          disabled={Boolean(fixed)}
          autoFocus
          onChange={(e) => setText(e.target.value)}
        />
        <span className="text-sm text-muted-foreground">MB/s</span>
        <Button type="submit" disabled={Boolean(fixed)}>
          Set
        </Button>
      </form>
      {d.rate_limit_mbps > 0 && !fixed ? (
        <Button variant="ghost" className="self-start" onClick={() => apply(0)}>
          Remove the limit
        </Button>
      ) : null}
    </div>
  );
}

// One way downloads can go out: its name, how it stands, and its switch
// when it has one.
function Way({
  name,
  status,
  problem = false,
  on,
  disabled = false,
  change,
}: {
  name: string;
  status: string;
  problem?: boolean;
  on?: boolean;
  disabled?: boolean;
  change?: (on: boolean) => Promise<boolean>;
}) {
  const [value, setValue] = useState(on ?? false);
  useEffect(() => setValue(on ?? false), [on]);
  return (
    <div className="flex items-center gap-3 py-2">
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{name}</div>
        <div className={cn('text-xs', problem ? 'text-warn-strong' : 'text-muted-foreground')}>{status}</div>
      </div>
      {change ? (
        <Switch
          checked={value}
          disabled={disabled}
          aria-label={name}
          onCheckedChange={async (next) => {
            setValue(next);
            if (!(await change(next))) setValue(!next);
          }}
        />
      ) : null}
    </div>
  );
}

// The ways downloads go out beside the line, as the Connections page groups
// them: a new IP from the router, proxies, and a VPN service (NordVPN today),
// each with its own switch. It follows the pool while open.
function Pool({ s }: { s: State }) {
  const { sources, why } = s.pool_summary;
  const r = s.reconnect;
  const router =
    r.blocked ?? (r.router ? `${r.router}, when a host holds this address back` : 'Looking for the router');
  const proxies = sources.find((x) => x.name === 'Proxies');
  const vpn = sources.find((x) => x.name === 'NordVPN');
  return (
    <div className="flex flex-col gap-2">
      <div className="-mt-1 divide-y">
        <Way
          name="New IP from the router"
          status={router}
          problem={Boolean(r.blocked)}
          on={r.auto}
          disabled={Boolean(r.blocked)}
          change={(on) => save({ reconnect: on }, null)}
        />
        <Way
          name="Proxies"
          status={proxies?.status ?? 'None set up yet'}
          on={proxies?.on ?? false}
          disabled={!proxies}
          change={(on) => save({ proxies_on: on }, null)}
        />
        <Way
          name="VPN"
          status={vpn ? `${vpn.name} · ${vpn.status}` : 'None set up yet'}
          problem={vpn?.problem}
          on={vpn?.on ?? false}
          disabled={!vpn}
          change={(on) => save({ nordvpn_on: on }, null)}
        />
      </div>
      {why ? <p className="text-sm text-warn-strong">{why}</p> : null}
    </div>
  );
}

export function StatusBar({ state: s }: { state: State | null }) {
  const theme = useTheme();
  if (!s) return <footer className="h-9 border-t bg-sidebar" />;
  const { downloading, waiting, left, slowed, unwrapping, need_you, ready } = s.counts;
  const clipboard = s.desktop && s.watch_clipboard;
  const pooled = s.pool.proxies + s.pool.vpn > 0;
  const index = THEMES.findIndex((t) => t.value === theme);
  const current = THEMES[index] ?? THEMES[0];
  const next = THEMES[(index + 1) % THEMES.length] ?? THEMES[0];
  const ThemeIcon = current.icon;
  return (
    <footer className="flex flex-wrap items-center gap-1 border-t bg-sidebar px-3 py-1 text-xs text-muted-foreground">
      <span className={cn(item, 'tabular-nums', s.speed && 'font-medium text-foreground')}>
        <ArrowDown aria-hidden className="size-3.5" />
        {bytes(s.speed)}/s
      </span>
      <span className={item}>{s.paused ? 'Paused' : `${downloading} downloading · ${waiting} waiting`}</span>
      {left ? (
        <span className={cn(item, 'tabular-nums')}>
          {bytes(left)} left
          {slowed ? ` · ${slowed} slowed by ${slowed === 1 ? 'its host' : 'their hosts'}` : ''}
          {!slowed && s.speed && !s.paused ? ` · ${roughly(left / s.speed)}` : ''}
        </span>
      ) : null}
      {s.reconnect.draining ? (
        <span className={item} title={s.reconnect.draining}>
          <LoaderCircle aria-hidden className="size-3.5 animate-spin" />
          New address once downloads finish: nothing new starts
        </span>
      ) : null}
      {/* Work on links before files show, and who waits for the user:
          steady entries rather than a toast for each. */}
      {unwrapping ? (
        <span className={item}>
          <LoaderCircle aria-hidden className="size-3.5 animate-spin" />
          Unwrapping {plural(unwrapping, 'link')}
        </span>
      ) : null}
      {ready ? (
        <button type="button" className={cn(item, 'hover:bg-accent')} onClick={() => go('wrappers')}>
          <Play aria-hidden className="size-3.5" />
          {ready} ready to start
        </button>
      ) : null}
      {need_you ? (
        <button
          type="button"
          className={cn(item, 'font-medium text-warn-strong hover:bg-accent')}
          onClick={() => go(s.counts.attention ? 'attention' : 'wrappers')}
        >
          <TriangleAlert aria-hidden className="size-3.5" />
          {need_you} {need_you === 1 ? 'needs' : 'need'} you
        </button>
      ) : null}
      <span className="flex-1" />
      <UpdateEntry s={s} />
      {clipboard ? (
        <Flyout
          title="Clipboard"
          on
          trigger={[<ClipboardList key="i" aria-hidden className="size-3.5" />, 'Watching the clipboard']}
        >
          {(d, close) => (
            <div className="flex flex-col gap-2">
              <p className="text-sm text-muted-foreground">
                Links to known hosts, files and pastes that you copy anywhere are added; other pages only if they list
                links.
              </p>
              <FlyoutSwitch
                label="Watch the clipboard"
                on={d.watch_clipboard}
                change={async (on) => {
                  const saved = await save({ watch_clipboard: on }, null);
                  if (saved && !on) close();
                  return saved;
                }}
              />
            </div>
          )}
        </Flyout>
      ) : null}
      <Flyout
        title="Speed limit"
        trigger={[
          <Gauge key="i" aria-hidden className="size-3.5" />,
          s.rate ? `${(s.rate / 1e6).toFixed(1)} MB/s cap` : 'No speed limit',
        ]}
      >
        {(d, close) => <SpeedLimit d={d} close={close} />}
      </Flyout>
      <Flyout
        title="Connections"
        on={pooled}
        action={(close) => (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              close();
              go('connections');
            }}
          >
            Settings
            <ChevronRight />
          </Button>
        )}
        trigger={[<Shield key="i" aria-hidden className="size-3.5" />, s.pool_summary.label]}
      >
        {() => <Pool s={s} />}
      </Flyout>
      <button
        type="button"
        title={`Theme: ${current.label} (click for ${next.label})`}
        onClick={() => setTheme(next.value)}
        className={cn(item, 'hover:bg-accent hover:text-foreground')}
      >
        <ThemeIcon aria-hidden className="size-3.5" />
        {current.label}
      </button>
    </footer>
  );
}
