import { FolderOpen, Pause, Play, RotateCw, Trash2, X } from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { HostPicker } from '@/components/host-picker';
import { IconButton } from '@/components/icon-button';
import { StatusGlyph } from '@/components/status-glyph';
import { Checkbox } from '@/components/ui/checkbox';
import { act, api, report } from '@/lib/api';
import type { Command } from '@/lib/bindings/Command';
import type { ItemView } from '@/lib/bindings/ItemView';
import type { State } from '@/lib/bindings/State';
import { caption, percent } from '@/lib/captions';
import { bytes, duration } from '@/lib/format';
import { pickMany, picking } from '@/lib/selection';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { NeedsYou, PasteLink } from '@/views/downloads/needs-you';

// The columns every row and the list's heading share.
export const COLUMNS = {
  lead: 'flex w-[52px] shrink-0 items-center gap-1',
  title: 'min-w-0 flex-[1_1_260px]',
  progress: 'min-w-[140px] flex-[0_1_240px]',
  rate: 'w-[110px] shrink-0 text-right text-xs tabular-nums text-muted-foreground',
  actions: 'flex min-w-[72px] shrink-0 justify-end gap-0.5',
};

const FILLS: Partial<Record<ItemView['status'], string>> = {
  done: 'bg-ok',
  error: 'bg-destructive',
  paused: 'bg-muted-foreground',
  queued: 'bg-muted-foreground',
  captcha: 'bg-muted-foreground',
};

export function Bar({ value, status }: { value: number; status: ItemView['status'] }) {
  return (
    <div className="h-1.5 overflow-hidden rounded-full bg-muted">
      <div
        className={cn('h-full rounded-full', FILLS[status] ?? 'bg-primary')}
        style={{ width: `${value.toFixed(1)}%` }}
      />
    </div>
  );
}

// The speed and what is left at it.
export function Rate({ speed, size, done }: { speed: number; size: number; done: number }) {
  if (!speed) return null;
  return (
    <>
      <div className="text-sm font-medium text-foreground">{bytes(speed)}/s</div>
      {size > done ? <div>{duration((size - done) / speed)} left</div> : null}
    </>
  );
}

// The row's lead: its state's glyph, which gives way to a checkbox while
// the row is hovered, picked, or any row is.
export function Lead({
  status,
  archive = false,
  checked,
  selecting,
  label,
  onCheck,
  before,
}: {
  status: ItemView['status'];
  archive?: boolean;
  checked: boolean | 'indeterminate';
  selecting: boolean;
  label: string;
  onCheck: (e: React.MouseEvent) => void;
  before?: ReactNode;
}) {
  const showCheck = selecting || checked !== false;
  return (
    <span className={COLUMNS.lead}>
      {before ?? <span className="w-6" />}
      <span className="grid size-4 place-items-center">
        <span className={cn('col-start-1 row-start-1', showCheck ? 'hidden' : 'group-hover/row:hidden')}>
          <StatusGlyph status={status} archive={archive} />
        </span>
        <Checkbox
          data-check
          checked={checked}
          aria-label={label}
          className={cn('col-start-1 row-start-1', showCheck ? 'flex' : 'hidden group-hover/row:flex')}
          onClick={(e) => {
            e.preventDefault();
            onCheck(e);
          }}
        />
      </span>
    </span>
  );
}

// The buttons the engine lists for a row (`commands`), each with its label.
function RowActions({ it, s }: { it: ItemView; s: State }) {
  const recycle = () =>
    api(`/api/item/${encodeURIComponent(it.id)}/remove`).then(
      () => toast(`Moved ${it.name} to the ${s.bin}`, 'success'),
      report,
    );
  const buttons: Partial<Record<Command, () => ReactNode>> = {
    pause: () => (
      <IconButton key="pause" label="Pause" onClick={() => act(it.id, 'pause')}>
        <Pause />
      </IconButton>
    ),
    start: () => (
      <IconButton key="start" label="Start" onClick={() => act(it.id, 'resume')}>
        <Play />
      </IconButton>
    ),
    resume: () => (
      <IconButton key="resume" label="Resume" onClick={() => act(it.id, 'resume')}>
        <Play />
      </IconButton>
    ),
    retry: () => (
      <IconButton key="retry" label="Retry" onClick={() => act(it.id, 'retry')}>
        <RotateCw />
      </IconButton>
    ),
    reveal: () => (
      <IconButton key="reveal" label="Show in folder" onClick={() => act(it.id, 'reveal')}>
        <FolderOpen />
      </IconButton>
    ),
    recycle: () => (
      <IconButton key="recycle" label={`Move to the ${s.bin}`} onClick={recycle}>
        <Trash2 />
      </IconButton>
    ),
    // Its partial file goes with it, so it asks twice, as Remove does for a
    // selection.
    remove_partial: () => (
      <ConfirmButton
        key="remove_partial"
        label="Remove"
        confirm="Delete partial file?"
        iconOnly
        icon={<X />}
        action={() => api(`/api/item/${encodeURIComponent(it.id)}/remove`)}
      />
    ),
    clear: () => (
      <IconButton key="clear" label="Clear from list" onClick={() => act(it.id, 'remove')}>
        <X />
      </IconButton>
    ),
    remove: () => (
      <IconButton key="remove" label="Remove" onClick={() => act(it.id, 'remove')}>
        <X />
      </IconButton>
    ),
  };
  return <div className={COLUMNS.actions}>{it.commands.map((c) => buttons[c]?.())}</div>;
}

// The host it comes from, starred when it is a preferred one, how many other
// hosts that are on could take over, and where it belongs; then why it
// failed or waits, or what the host wants of the user.
function Subtitle({ it }: { it: ItemView }) {
  // The host part, which chooses the host when the file has several.
  const others = it.other_hosts ? ` · ${it.other_hosts} other ${it.other_hosts === 1 ? 'host' : 'hosts'}` : '';
  const host = `${it.preferred_host || it.download_from ? '★ ' : ''}${it.shown_host}${others}`;
  const rest: string[] = [];
  if (it.package) rest.push(it.package);
  if (it.source_name) rest.push(`from ${it.source_name}`);
  const reason = ['error', 'waiting', 'captcha'].includes(it.status) ? it.error_shown : '';
  const tone = it.status === 'error' ? 'text-destructive' : it.status === 'captcha' ? 'text-warn-strong' : '';
  return (
    <div className="truncate text-xs text-muted-foreground" title={[host, ...rest, reason].filter(Boolean).join('\n')}>
      <HostPicker ids={[it.id]} hosts={it.mirrors} chosen={it.download_from} label={host} />
      {rest.length ? ` · ${rest.join(' · ')}` : null}
      {reason ? <span className={tone}> · {reason}</span> : null}
    </div>
  );
}

export function ItemRow({
  it,
  s,
  order,
  picked,
  selecting,
  member = false,
}: {
  it: ItemView;
  s: State;
  order: string[];
  picked: boolean;
  selecting: boolean;
  member?: boolean;
}) {
  const [pasting, setPasting] = useState(false);
  const needs = it.status === 'captcha';
  const hint = s.desktop
    ? 'Open here, solve the page and press its download button: Haul takes the download over.'
    : 'Open the page, solve it, then copy the final download link and paste it here.';
  return (
    // A click picks the row once picking has started (Ctrl adds or drops it,
    // Shift picks a range); its own buttons, links and fields keep their
    // clicks.
    // biome-ignore lint/a11y/useKeyWithClickEvents: the row's checkbox is its keyboard way in
    <li
      className={cn(
        'group/row border-t px-4 py-2.5 transition-colors',
        member && 'bg-muted/35 pl-11',
        needs ? 'bg-warn-soft' : 'hover:bg-muted/50',
        picked && 'bg-accent hover:bg-accent',
      )}
      onMouseDown={(e) => e.shiftKey && e.preventDefault()}
      onClick={(e) => {
        if ((e.target as HTMLElement).closest('button, a, input, textarea, form, [role=menu]')) return;
        if (picking(e)) pickMany([it.id], order, { toggle: true, range: e.shiftKey });
      }}
    >
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <Lead
          status={it.status}
          checked={picked}
          selecting={selecting}
          label={`Select ${it.name}`}
          onCheck={(e) => pickMany([it.id], order, { toggle: true, range: e.shiftKey })}
        />
        <div className={COLUMNS.title}>
          <div className="truncate font-medium" title={it.name}>
            {it.name}
          </div>
          <Subtitle it={it} />
        </div>
        <div className={COLUMNS.progress}>
          {needs ? (
            <NeedsYou w={it} desktop={s.desktop} paste={() => setPasting(true)} hint={hint} />
          ) : (
            <>
              <Bar value={percent(it)} status={it.status} />
              <div className="mt-1.5 truncate text-xs text-muted-foreground tabular-nums">{caption(it, s.paused)}</div>
            </>
          )}
        </div>
        <div className={COLUMNS.rate}>
          <Rate speed={it.speed} size={it.size} done={it.done} />
        </div>
        <RowActions it={it} s={s} />
      </div>
      {pasting && needs ? (
        <div className="mt-2.5 pl-[68px]">
          <PasteLink w={it} close={() => setPasting(false)} done={`Using your link for ${it.name}`} />
        </div>
      ) : null}
    </li>
  );
}
