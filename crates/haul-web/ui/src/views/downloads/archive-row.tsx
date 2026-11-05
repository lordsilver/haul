import { ChevronDown, ChevronRight, FileArchive, KeyRound, Pause, Play, RotateCw, Trash2, X } from 'lucide-react';
import { type FormEvent, useRef, useState } from 'react';
import { FileName } from '@/components/file-name';
import { HostNames, HostPicker, hostLabel, offHosts } from '@/components/host-picker';
import { IconButton } from '@/components/icon-button';
import { Infobar } from '@/components/infobar';
import { SecretInput } from '@/components/secret-input';
import { Button } from '@/components/ui/button';
import { api, bulk, report } from '@/lib/api';
import type { ArchiveView } from '@/lib/bindings/ArchiveView';
import type { Command } from '@/lib/bindings/Command';
import type { ItemView } from '@/lib/bindings/ItemView';
import type { State } from '@/lib/bindings/State';
import { archiveCaption } from '@/lib/captions';
import { bytes } from '@/lib/format';
import { pickMany, picking } from '@/lib/selection';
import { cn } from '@/lib/utils';
import { toggleArchive } from '@/views/downloads/entries';
import { Bar, COLUMNS, type CommandButton, CommandButtons, InlineRate, Lead, ROW, Rate } from '@/views/downloads/row';

// The password of an encrypted archive, typed in: Haul keeps it with the
// archive and extracts with it.
function PasswordBar({ a, input }: { a: ArchiveView; input: React.RefObject<HTMLInputElement | null> }) {
  const [password, setPassword] = useState('');
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api(`/api/item/${encodeURIComponent(a.ids[0] ?? '')}/extract`, { password });
      setPassword('');
      input.current?.blur();
    } catch (error) {
      report(error);
    }
  };
  const why =
    a.extract_error === 'none of the passwords fits'
      ? 'None of the passwords fits this archive.'
      : 'This archive is encrypted.';
  return (
    <div className="mt-2.5 flex flex-col gap-1.5 pl-[68px]">
      <Infobar
        kind="caution"
        actions={
          <form className="flex items-center gap-2" onSubmit={submit}>
            <SecretInput
              ref={input}
              required
              value={password}
              placeholder="Password"
              aria-label={`Password for ${a.name}`}
              onChange={(e) => setPassword(e.target.value)}
            />
            <Button type="submit" size="sm">
              Extract
            </Button>
          </form>
        }
      >
        {why}
      </Infobar>
      <p className="text-xs text-muted-foreground">
        Haul keeps the password with this archive. Passwords that open many archives go under Settings › Archive
        passwords, tried on every one.
      </p>
    </div>
  );
}

// The parts of an archive whose own buttons do one of `commands`.
function partsWith(a: ArchiveView, s: State, ...commands: Command[]) {
  const ids = new Set(a.ids);
  return s.items.filter((i) => ids.has(i.id) && i.commands.some((c) => commands.includes(c))).map((i) => i.id);
}

// Pause, resume, retry or remove the parts together, and extract them once
// they are all done.
function ArchiveActions({ a, s, focusPassword }: { a: ArchiveView; s: State; focusPassword: () => void }) {
  const extract = () => api(`/api/item/${encodeURIComponent(a.ids[0] ?? '')}/extract`).catch(report);
  const remove = () => bulk(a.ids, 'remove');
  const resume = () => bulk(partsWith(a, s, 'start', 'resume'), 'resume');
  const buttons: Partial<Record<Command, CommandButton>> = {
    extract: { label: 'Extract', icon: FileArchive, action: extract },
    extract_again: { label: 'Extract again', icon: FileArchive, action: extract },
    pause: { label: 'Pause the parts', icon: Pause, action: () => bulk(partsWith(a, s, 'pause'), 'pause') },
    start: { label: 'Start the parts', icon: Play, action: resume },
    resume: { label: 'Resume the parts', icon: Play, action: resume },
    retry: { label: 'Retry the failed parts', icon: RotateCw, action: () => bulk(partsWith(a, s, 'retry'), 'retry') },
    password: { label: 'Enter the password', icon: KeyRound, action: async () => focusPassword() },
    recycle: { label: `Move the parts to the ${s.bin}`, icon: Trash2, action: remove },
    // Partial files go with them, so it asks twice, as a part's own Remove does.
    remove_partial: { label: 'Remove the parts', confirm: 'Delete partial files?', icon: X, action: remove },
    clear: { label: 'Clear the parts from the list', icon: X, action: remove },
    remove: { label: 'Remove the parts', icon: X, action: remove },
  };
  return <CommandButtons commands={a.commands} buttons={buttons} />;
}

// An archive in parts as one row, like JDownloader's packages: it sums up
// the whole archive, and opens to show the parts this view has.
export function ArchiveRow({
  a,
  members,
  s,
  order,
  picked,
  selecting,
  open,
  searching,
}: {
  a: ArchiveView;
  members: ItemView[];
  s: State;
  order: string[];
  picked: Set<string>;
  selecting: boolean;
  open: boolean;
  searching: boolean;
}) {
  const password = useRef<HTMLInputElement>(null);
  const ids = members.map((i) => i.id);
  const count = ids.filter((id) => picked.has(id)).length;
  const checked = count === 0 ? false : count === ids.length ? true : 'indeterminate';
  const lead = [`${a.ids.length} parts`, a.size ? bytes(a.size) : null].filter(Boolean).join(' · ');
  // The host part, which chooses the host for every part when they have
  // several; a host that is off is greyed out.
  const off = offHosts(s);
  const others = a.other_hosts ? `${a.other_hosts} other ${a.other_hosts === 1 ? 'host' : 'hosts'}` : null;
  const host = [a.hosts.join(', '), others].filter(Boolean).join(' · ');
  const rest = [a.package || null, a.source_name ? `from ${a.source_name}` : null].filter(Boolean).join(' · ');
  const sub = [lead, host, rest, a.slowed].filter(Boolean).join(' · ');
  // Once every part is done, the bar and its caption are the unpacking's.
  const share = a.extract === 'extracting' ? (a.extract_progress ?? 0) : a.share;
  const label = open ? 'Hide the parts' : 'Show the parts';
  const asking = a.commands.includes('password');
  return (
    // biome-ignore lint/a11y/useKeyWithClickEvents: the row's checkbox is its keyboard way in
    <li
      className={cn(
        'group/row border-t px-4 py-2.5 transition-colors hover:bg-muted/50',
        checked === true && 'bg-accent hover:bg-accent',
      )}
      onMouseDown={(e) => e.shiftKey && e.preventDefault()}
      onClick={(e) => {
        if ((e.target as HTMLElement).closest('button, a, input, form')) return;
        if (picking(e)) pickMany(ids, order, { toggle: true, range: e.shiftKey });
      }}
    >
      <div className={ROW}>
        <Lead
          status={a.status}
          archive
          checked={checked}
          selecting={selecting}
          label={`Select the parts of ${a.name}`}
          onCheck={(e) => pickMany(ids, order, { toggle: true, range: e.shiftKey })}
          before={
            // A search opens every archive, so the button has nothing to do;
            // it keeps its place, so the title does not jump while typing.
            <IconButton
              label={label}
              size="icon-sm"
              aria-expanded={open}
              className={cn(searching && 'invisible')}
              onClick={() => toggleArchive(a.key)}
            >
              {open ? <ChevronDown className="size-5" /> : <ChevronRight className="size-5" />}
            </IconButton>
          }
        />
        <div className={COLUMNS.title}>
          <FileName name={a.name} />
          <div className="truncate text-xs text-muted-foreground" title={sub}>
            {lead}
            {host ? ' · ' : null}
            <HostPicker
              ids={ids}
              hosts={a.all_hosts}
              off={off}
              chosen={a.download_from}
              label={hostLabel(
                <>
                  <HostNames hosts={a.hosts} off={off} />
                  {a.hosts.length && others ? ' · ' : null}
                  {others}
                </>,
                { picked: Boolean(a.download_from), preferred: false },
              )}
            />
            {rest ? ` · ${rest}` : null}
          </div>
        </div>
        <div className={COLUMNS.progress}>
          <Bar value={Math.min(100, share * 100)} status={a.extract === 'failed' ? 'error' : a.status} />
          <div
            className={cn(
              'mt-1.5 truncate text-xs tabular-nums',
              a.extract === 'failed' ? 'text-destructive' : 'text-muted-foreground',
            )}
            title={
              a.extract === 'failed' ? a.extract_error : a.size ? `${bytes(a.done)} of ${bytes(a.size)}` : undefined
            }
          >
            {archiveCaption(a)}
            <InlineRate speed={a.speed} />
          </div>
        </div>
        <div className={COLUMNS.rate}>
          <Rate speed={a.speed} size={a.size} done={a.done} slowed={Boolean(a.slowed)} />
        </div>
        <ArchiveActions a={a} s={s} focusPassword={() => password.current?.focus()} />
      </div>
      {asking ? <PasswordBar a={a} input={password} /> : null}
    </li>
  );
}
