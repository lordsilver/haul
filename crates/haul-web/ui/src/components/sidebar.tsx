import {
  CircleArrowDown,
  CircleCheck,
  CirclePause,
  CircleX,
  FolderOpen,
  Globe,
  Link,
  List,
  type LucideIcon,
  Server,
  Settings,
  TriangleAlert,
} from 'lucide-react';
import type { ReactNode } from 'react';
import { Count, type CountTone } from '@/components/count';
import { Separator } from '@/components/ui/separator';
import { openPlace } from '@/lib/api';
import type { State } from '@/lib/bindings/State';
import { type ViewKey, go } from '@/lib/route';
import { clearSelection } from '@/lib/selection';
import { cn } from '@/lib/utils';

function NavItem({
  icon: Icon,
  iconClass,
  label,
  current,
  onClick,
  title,
  children,
}: {
  icon: LucideIcon;
  iconClass?: string;
  label: string;
  current?: boolean;
  onClick: () => void;
  title?: string;
  children?: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-current={current ? 'page' : undefined}
      title={title}
      onClick={onClick}
      className={cn(
        'flex h-[34px] w-full items-center gap-2.5 rounded-lg px-2.5 text-left text-sm outline-none hover:bg-accent focus-visible:ring-3 focus-visible:ring-ring/50',
        current && 'bg-accent font-medium',
      )}
    >
      <Icon aria-hidden className={cn('size-4 shrink-0', iconClass)} />
      <span className="truncate">{label}</span>
      {children}
    </button>
  );
}

const Heading = ({ children }: { children: ReactNode }) => (
  <div className="px-2.5 pt-3.5 pb-1 text-xs font-medium text-muted-foreground first:pt-1.5">{children}</div>
);

// The lists of downloads with their counts, colored by what their state
// means; where downloads came from; and the pages.
export function Sidebar({ state, view }: { state: State | null; view: ViewKey }) {
  const c = state?.counts;
  const open = (next: ViewKey) => () => {
    if (next !== view) clearSelection();
    go(next);
  };
  const lists: [ViewKey, string, LucideIcon, string | undefined, number, CountTone][] = c
    ? [
        ['all', 'All downloads', List, undefined, c.all, 'plain'],
        ['active', 'Active', CircleArrowDown, 'text-info', c.active, c.downloading ? 'info' : 'plain'],
        ['paused', 'Paused', CirclePause, undefined, c.paused, 'muted'],
        ['done', 'Completed', CircleCheck, 'text-ok', c.done, 'ok'],
        ['attention', 'Needs you', TriangleAlert, 'text-warn', c.attention, 'warn'],
        ['failed', 'Failed', CircleX, 'text-destructive', c.failed, 'bad'],
      ]
    : [];
  const wrappersTone: CountTone =
    c?.wrappers_alert === 'critical' ? 'bad' : c?.wrappers_alert === 'attention' ? 'warn' : 'plain';
  return (
    <nav
      aria-label="Views"
      className="flex w-60 shrink-0 flex-col gap-1 overflow-y-auto border-r bg-sidebar px-3 py-3.5 max-md:w-52"
    >
      <div className="flex items-center gap-2.5 px-2 pt-1 pb-3.5">
        <img src="/icon.svg" alt="" className="size-[30px]" />
        <div className="flex flex-col leading-tight">
          <span className="font-semibold">Haul</span>
          <span className="text-xs text-muted-foreground">{state?.version}</span>
        </div>
      </div>
      <Heading>Downloads</Heading>
      {lists.map(([key, label, icon, iconClass, n, tone]) => (
        <NavItem key={key} icon={icon} iconClass={iconClass} label={label} current={view === key} onClick={open(key)}>
          <Count n={n} tone={n ? tone : 'plain'} />
        </NavItem>
      ))}
      <Heading>Sources</Heading>
      <NavItem icon={Link} label="Wrappers" current={view === 'wrappers'} onClick={open('wrappers')}>
        <Count n={c?.wrappers ?? 0} tone={wrappersTone} />
      </NavItem>
      <div className="min-h-4 flex-1" />
      <NavItem
        icon={FolderOpen}
        label="Downloads"
        title={state ? `Open ${state.download_dir}` : undefined}
        onClick={() => openPlace('downloads')}
      />
      <Separator className="mx-1 my-1.5 data-horizontal:w-auto" />
      <NavItem icon={Server} label="Sites" current={view === 'hosts'} onClick={open('hosts')} />
      <NavItem icon={Globe} label="Connections" current={view === 'connections'} onClick={open('connections')} />
      <NavItem icon={Settings} label="Settings" current={view === 'settings'} onClick={open('settings')} />
    </nav>
  );
}
