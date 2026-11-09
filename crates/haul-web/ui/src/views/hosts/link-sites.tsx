// The wrapper and paste sites, listed apart from the file hosts.
import { Wrench } from 'lucide-react';
import { useState } from 'react';
import { IconButton } from '@/components/icon-button';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import type { HostView } from '@/lib/bindings/HostView';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { cn } from '@/lib/utils';
import { HEADINGS, KIND_ICONS, Pills, freeAccess, yourLinks } from '@/views/hosts/pills';
import { HostSettings, HostSwitch, RouteBar } from '@/views/hosts/settings';

// Wrappers and pastebins are looked up now and then, unlike the file hosts the
// page is for: each kind opens from a button rather than fill the list.
export const LINK_SITES = ['wrapper', 'paste'] as const;

export const isLinkSite = (h: HostView) => (LINK_SITES as readonly string[]).includes(h.group);

export function LinkSite({ h, s, d }: { h: HostView; s: State; d: SettingsView | null }) {
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

export function LinkSitesButton({
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
