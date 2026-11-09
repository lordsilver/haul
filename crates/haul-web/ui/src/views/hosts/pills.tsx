// What a host card shows of a host at a glance: pills and lines of words.
import { ClipboardList, Clock, Folder, Gauge, Globe, Info, Link2, Server, Shield, Star } from 'lucide-react';
import type { ReactNode } from 'react';
import { Hint } from '@/components/hint';
import type { HostView } from '@/lib/bindings/HostView';
import { bytes, duration, ordinal, plural } from '@/lib/format';
import { cn } from '@/lib/utils';

// Each kind's icon; a host that can move shows its grip instead.
export const KIND_ICONS: Record<HostView['kind'], typeof Server> = {
  host: Server,
  wrapper: Link2,
  paste: ClipboardList,
  site: Globe,
};

export function Pill({
  tone = 'plain',
  title,
  children,
}: {
  tone?: 'plain' | 'caution' | 'star';
  title?: string;
  children: ReactNode;
}) {
  return (
    <Hint text={title}>
      <span
        // A pill that explains itself can be reached to hear why.
        tabIndex={title ? 0 : undefined}
        className={cn(
          'inline-flex h-6 items-center gap-1 rounded-full border px-2 text-xs outline-none focus-visible:ring-3 focus-visible:ring-ring/50 [&_svg]:size-3',
          tone === 'plain' && 'text-muted-foreground',
          tone === 'caution' && 'border-warn-line bg-warn-soft text-warn-strong',
          tone === 'star' && 'border-transparent bg-info-soft text-info-strong',
        )}
      >
        {children}
      </span>
    </Hint>
  );
}

// What a free user gets there, in one form for every host: an account set
// replaces it, and a host with nothing known says so.
export function freeAccess(h: HostView): string {
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
export function yourDownloads(h: HostView): string {
  const used: string[] = [];
  if (h.bytes) used.push(`${bytes(h.bytes)}`);
  if (h.ok) used.push(plural(h.ok, 'file'));
  if (h.speed) used.push(`${bytes(h.speed)}/s`);
  if (h.fails) used.push(`${h.fails} failed`);
  if (h.quota) used.push(`${bytes(h.quota.used)} today`);
  return used.join(' · ') || 'Not used yet';
}

// A wrapper's or a paste's own: pages opened, as they hold no files.
export function yourLinks(h: HostView): string {
  const used: string[] = [];
  if (h.ok) used.push(`Opened ${plural(h.ok, 'time')}`);
  if (h.fails) used.push(`${h.fails} failed`);
  return used.join(' · ') || 'Not used yet';
}

export function Pills({ h, proxy }: { h: HostView; proxy: boolean }) {
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

export const HEADINGS: Record<HostView['group'], [string, string]> = {
  preferred: ['Preferred hosts, in order', 'A file downloads from the first of these that has it.'],
  other: ['Other hosts', 'Used when no preferred host has the file, the fastest first; one switched off never.'],
  wrapper: ['Wrappers', 'Sites that hold download links on file hosts (containers, link protectors).'],
  paste: ['Pastebins', 'Paste sites whose pages list links.'],
  site: ['Other sites', 'Sites Haul has no code for that you gave a setting; the rest are used without one.'],
};
