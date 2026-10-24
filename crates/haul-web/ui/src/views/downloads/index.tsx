import {
  CircleArrowDown,
  CircleCheck,
  CirclePause,
  CircleX,
  Download,
  Eraser,
  type LucideIcon,
  Pause,
  Play,
  Plus,
  RotateCw,
  Search,
  Trash2,
  TriangleAlert,
  X,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { HostPicker } from '@/components/host-picker';
import { IconButton } from '@/components/icon-button';
import { Infobar } from '@/components/infobar';
import { Button } from '@/components/ui/button';
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty';
import { Input } from '@/components/ui/input';
import { addOpen, openAdd } from '@/lib/add';
import { api, bulk, report } from '@/lib/api';
import type { ItemView } from '@/lib/bindings/ItemView';
import type { State } from '@/lib/bindings/State';
import { plural } from '@/lib/format';
import { type ListKey, go } from '@/lib/route';
import { clearSelection, keepOnly, selectAll, useSelection } from '@/lib/selection';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { ArchiveRow } from '@/views/downloads/archive-row';
import { entriesOf, inList, matchesQuery, orderOf, useExpanded } from '@/views/downloads/entries';
import { COLUMNS, ItemRow } from '@/views/downloads/row';

const TITLES: Record<ListKey, string> = {
  all: 'Downloads',
  active: 'Active',
  paused: 'Paused',
  done: 'Completed',
  attention: 'Needs you',
  failed: 'Failed',
};

const EMPTY: Record<Exclude<ListKey, 'all'>, [LucideIcon, string, string]> = {
  active: [CircleArrowDown, 'Nothing is downloading', 'Downloads that are running or waiting their turn show up here.'],
  attention: [
    TriangleAlert,
    'No host is waiting for you',
    'When a host wants a captcha or a browser step, the download waits here.',
  ],
  failed: [CircleX, 'No failed downloads', 'Downloads whose mirrors all failed show up here.'],
  paused: [CirclePause, 'Nothing is paused', 'Paused downloads keep their progress and resume where they stopped.'],
  done: [CircleCheck, 'No completed downloads yet', 'Finished files show up here until you clear them.'],
};

// What the list says when it has no rows. With no downloads at all it says
// how to add some; it is no box to drop into, since the whole window takes
// drops.
function Nothing({ s, view, query }: { s: State; view: ListKey; query: string }) {
  const start = !query && (!s.items.length || view === 'all');
  const [Icon, title, text] = query
    ? [Search, 'No matches', `Nothing here matches “${query}”.`]
    : start || view === 'all'
      ? [
          Download,
          'Drop links or files anywhere',
          'Or paste links with Ctrl+V. Files can be .dlc containers or text files with links in them.',
        ]
      : EMPTY[view];
  return (
    <Empty className={cn('flex-1', !start && 'border')}>
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <Icon />
        </EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        <EmptyDescription>{text}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

// App-wide state only: a download that needs the user has its own row and
// the Needs you count.
function Banner({ s, offline, updated }: { s: State; offline: boolean; updated: boolean }) {
  return (
    <>
      {offline ? (
        <Infobar kind="critical" title="Lost the connection to Haul.">
          Reconnecting…
        </Infobar>
      ) : null}
      {updated ? (
        <Infobar
          kind="info"
          title="Haul was updated."
          actions={
            <Button size="sm" variant="outline" onClick={() => location.reload()}>
              <RotateCw />
              Reload
            </Button>
          }
        >
          Reload this page to use the new version.
        </Infobar>
      ) : null}
      {s.config_error ? (
        <Infobar
          kind="critical"
          title="The settings file has an error."
          actions={
            <Button size="sm" variant="outline" onClick={() => go('settings')}>
              Show
            </Button>
          }
        >
          The previous settings stay in effect until it is fixed.
        </Infobar>
      ) : null}
      {/* Paused matters only while it holds something back; with nothing to
          hold, the status bar says it. */}
      {s.counts.held ? (
        <Infobar
          kind="info"
          title="Downloads are paused."
          actions={
            <Button size="sm" variant="outline" onClick={() => api('/api/global/resume').catch(report)}>
              <Play />
              Unpause
            </Button>
          }
        >
          Nothing new starts until you resume.
        </Infobar>
      ) : null}
    </>
  );
}

// The buttons for the picked rows: each acts on those it applies to.
function SelectionCommands({ picked, s }: { picked: ItemView[]; s: State }) {
  const where = (...commands: string[]) =>
    picked.filter((i) => i.commands.some((c) => commands.includes(c))).map((i) => i.id);
  const retry = where('retry');
  const pause = where('pause');
  const resumable = picked.filter((i) => i.commands.some((c) => c === 'start' || c === 'resume'));
  // Unfinished downloads lose their partial files, so that asks twice.
  const losing = where('remove_partial').length;
  const recycling = picked.filter((i) => i.commands.includes('recycle') && !i.existing).length;
  const remove = async () => {
    const n = await bulk(
      picked.map((i) => i.id),
      'remove',
    );
    clearSelection();
    toast(
      recycling ? `Removed ${plural(n, 'download')}, files to the ${s.bin}` : `Removed ${plural(n, 'download')}`,
      'success',
    );
  };
  const label = `Remove ${picked.length}`;
  // Every host among the picked rows, to download them from one of.
  const hosts = [...new Set(picked.flatMap((i) => i.mirrors))].sort();
  const choices = new Set(picked.map((i) => i.download_from));
  return (
    <>
      <span role="status" className="px-2 text-sm text-muted-foreground">
        {picked.length} selected
      </span>
      {retry.length ? (
        <Button variant="ghost" onClick={() => bulk(retry, 'retry')}>
          <RotateCw />
          Retry
        </Button>
      ) : null}
      {pause.length ? (
        <Button variant="ghost" onClick={() => bulk(pause, 'pause')}>
          <Pause />
          Pause
        </Button>
      ) : null}
      {resumable.length ? (
        <Button
          variant="ghost"
          onClick={() =>
            bulk(
              resumable.map((i) => i.id),
              'resume',
            )
          }
        >
          <Play />
          {resumable.every((i) => i.not_started) ? 'Start' : 'Resume'}
        </Button>
      ) : null}
      {hosts.length > 1 ? (
        <HostPicker
          bar
          ids={picked.map((i) => i.id)}
          hosts={hosts}
          chosen={choices.size === 1 ? [...choices][0] : undefined}
          label="Download from"
        />
      ) : null}
      {losing || recycling ? (
        <ConfirmButton
          data-role="remove"
          label={label}
          confirm={
            losing ? `Delete ${plural(losing, 'partial file')}?` : `Move ${plural(recycling, 'file')} to the ${s.bin}?`
          }
          icon={<Trash2 />}
          action={remove}
        />
      ) : (
        <Button data-role="remove" variant="ghost" onClick={() => remove().catch(report)}>
          <Trash2 />
          {label}
        </Button>
      )}
      <IconButton label="Clear selection" onClick={clearSelection}>
        <X />
      </IconButton>
    </>
  );
}

function ListCommands({ s, view }: { s: State; view: ListKey }) {
  const failed = s.counts.failed;
  // Downloads in this view that wait for the user: never started, or paused.
  const waiting = s.items.filter((i) => i.list === 'paused' && inList(i, view));
  const fresh = waiting.length > 0 && waiting.every((i) => i.not_started);
  return (
    <>
      <Button onClick={() => openAdd()}>
        <Plus />
        Add links
      </Button>
      {/* Starts what waits for the user; the pause switch below does not. */}
      {waiting.length ? (
        <Button
          variant="ghost"
          onClick={() =>
            bulk(
              waiting.map((i) => i.id),
              'resume',
            )
          }
        >
          <Play />
          {fresh ? 'Start' : 'Resume'} {waiting.length}
        </Button>
      ) : null}
      <Button variant="outline" onClick={() => api(`/api/global/${s.paused ? 'resume' : 'pause'}`).catch(report)}>
        {s.paused ? <Play /> : <Pause />}
        {s.paused ? 'Unpause' : 'Pause all'}
      </Button>
      <Button variant="ghost" disabled={!failed} onClick={() => api('/api/global/retry_failed').catch(report)}>
        <RotateCw />
        Retry failed
      </Button>
      {/* In the Failed view the eraser clears failed downloads, partial files
          and all, so it asks for a second click; elsewhere it tidies
          finished ones. */}
      {view === 'failed' ? (
        failed ? (
          <ConfirmButton
            label="Clear failed"
            confirm={`Clear ${plural(failed, 'download')}?`}
            icon={<Eraser />}
            action={() => api('/api/global/clear_failed')}
          />
        ) : (
          <Button variant="ghost" disabled>
            <Eraser />
            Clear failed
          </Button>
        )
      ) : (
        <Button variant="ghost" disabled={!s.counts.done} onClick={() => api('/api/global/clear_done').catch(report)}>
          <Eraser />
          Clear completed
        </Button>
      )}
    </>
  );
}

export function Downloads({
  s,
  view,
  offline,
  updated,
}: {
  s: State;
  view: ListKey;
  offline: boolean;
  updated: boolean;
}) {
  const [typed, setTyped] = useState('');
  const query = typed.trim().toLowerCase();
  const search = useRef<HTMLInputElement>(null);
  const picked = useSelection();
  const expanded = useExpanded();
  const visible = useMemo(() => s.items.filter((i) => inList(i, view) && matchesQuery(i, query)), [s, view, query]);
  const entries = useMemo(() => entriesOf(visible, s), [visible, s]);
  const order = useMemo(() => orderOf(entries), [entries]);
  const pickedItems = s.items.filter((i) => picked.has(i.id));
  const selecting = picked.size > 0;

  useEffect(() => keepOnly(new Set(visible.map((i) => i.id))), [visible]);

  useEffect(() => {
    const keys = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement).closest?.('input, textarea, [role=dialog], [role=menu]') || addOpen()) return;
      if (e.key === '/') {
        e.preventDefault();
        search.current?.focus();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
        e.preventDefault();
        selectAll(order);
      } else if (e.key === 'Escape' && picked.size) {
        clearSelection();
      } else if (e.key === 'Delete' && picked.size) {
        // Same as the Remove button, second press included where it asks.
        document.querySelector<HTMLElement>('[data-role=remove]')?.click();
      }
    };
    document.addEventListener('keydown', keys);
    return () => document.removeEventListener('keydown', keys);
  }, [order, picked]);

  return (
    <>
      <header className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <h1 className="mr-auto text-2xl font-semibold tracking-tight">{TITLES[view]}</h1>
        <div className="flex flex-wrap items-center gap-2">
          {pickedItems.length ? <SelectionCommands picked={pickedItems} s={s} /> : <ListCommands s={s} view={view} />}
          <div className="relative w-56">
            <Search
              aria-hidden
              className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              ref={search}
              type="search"
              value={typed}
              placeholder="Search"
              aria-label="Search downloads"
              className="pl-9"
              onChange={(e) => setTyped(e.target.value)}
              onKeyDown={(e) => {
                if (e.key !== 'Escape') return;
                setTyped('');
                search.current?.blur();
              }}
            />
          </div>
        </div>
      </header>
      <Banner s={s} offline={offline} updated={updated} />
      {visible.length ? (
        // A click on the list's empty space, outside any row, drops the
        // selection.
        // biome-ignore lint/a11y/useKeyWithClickEvents: Escape does the same from the keyboard
        <section
          aria-label={TITLES[view]}
          className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto rounded-xl border bg-background shadow-xs"
          onClick={(e) => {
            if (!(e.target as HTMLElement).closest('li, button, input, a')) clearSelection();
          }}
        >
          <div className="sticky top-0 z-10 flex items-center gap-x-4 bg-background px-4 py-2.5 text-xs font-medium text-muted-foreground max-lg:hidden">
            <span className={COLUMNS.lead} />
            <span className={COLUMNS.title}>Name</span>
            <span className={COLUMNS.progress}>Progress</span>
            <span className={COLUMNS.rate}>Speed</span>
            <span className={COLUMNS.actions} />
          </div>
          <ul>
            {entries.map((e) => {
              if (!('members' in e)) {
                return (
                  <ItemRow
                    key={e.it.id}
                    it={e.it}
                    s={s}
                    order={order}
                    picked={picked.has(e.it.id)}
                    selecting={selecting}
                  />
                );
              }
              const open = Boolean(query) || expanded.has(e.key);
              return [
                <ArchiveRow
                  key={e.key}
                  a={e.archive}
                  members={e.members}
                  s={s}
                  order={order}
                  picked={picked}
                  selecting={selecting}
                  open={open}
                  searching={Boolean(query)}
                />,
                ...(open
                  ? e.members.map((it) => (
                      <ItemRow
                        key={it.id}
                        it={it}
                        s={s}
                        order={order}
                        picked={picked.has(it.id)}
                        selecting={selecting}
                        member
                      />
                    ))
                  : []),
              ];
            })}
          </ul>
        </section>
      ) : (
        <Nothing s={s} view={view} query={query} />
      )}
    </>
  );
}
