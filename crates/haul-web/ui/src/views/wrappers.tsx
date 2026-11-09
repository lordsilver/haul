import { Eraser, Link, Play, RotateCw, X } from 'lucide-react';
import { useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { HostName, offHosts } from '@/components/host-picker';
import { EmptyState, PageHeader, PageScroll, SearchBox } from '@/components/page';
import { Button } from '@/components/ui/button';
import { act, wrappersBulk } from '@/lib/api';
import type { State } from '@/lib/bindings/State';
import type { WrapperCard } from '@/lib/bindings/WrapperCard';
import { partsSummary } from '@/lib/captions';
import { wrappers } from '@/lib/client';
import { duration, plural } from '@/lib/format';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { NeedsYou, PasteLink } from '@/views/downloads/needs-you';

// Cards' wrappers together, in one request: their files counted once, since
// a file two of them list starts with the first.
async function startAll(cards: WrapperCard[]) {
  const r = await wrappersBulk(
    cards.flatMap((c) => c.ids),
    'start',
  );
  if (r?.started) {
    const from = cards.length === 1 && cards[0] ? ` from ${cards[0].name}` : '';
    toast(`Started ${plural(r.started, 'file')}${from}`, 'success');
  }
}

// The card going is what says it worked.
const removeAll = (cards: WrapperCard[]) =>
  wrappersBulk(
    cards.flatMap((c) => c.ids),
    'remove',
  );

// Takes a host's links off the card's files; a file only on it goes.
const dropHost = (card: WrapperCard, host: string) =>
  wrappers.dropHost(card.ids, host).then((r) => {
    toast(`Dropped ${host} from ${plural(r.dropped, 'file')}`, 'success');
  });

// The hosts the card's files are on, each with a button that drops it; a
// host that is off is greyed out.
function Hosts({ card, s }: { card: WrapperCard; s: State }) {
  const off = offHosts(s);
  return (
    <div className="mt-1.5 flex flex-wrap gap-1.5">
      {card.hosts.map((host) => (
        <span key={host} className="inline-flex items-center gap-0.5 rounded-full border bg-background pl-2.5 text-xs">
          <HostName host={host} off={off.has(host)} />
          <ConfirmButton
            label={`Drop ${host}: its links go from these files`}
            confirm="Drop its links?"
            iconOnly
            size="icon-xs"
            className="rounded-full"
            icon={<X />}
            action={() => dropHost(card, host)}
          />
        </span>
      ))}
    </div>
  );
}

// What a wrapper is doing, in a few words; once unwrapped, how its files
// stand.
function status(card: WrapperCard): string {
  switch (card.status) {
    case 'unwrapping':
      return 'Unwrapping…';
    case 'waiting':
      return card.wait ? `Trying again in ${duration(card.wait)}` : 'Waiting to try again';
    case 'passing':
      return 'Haul opens its page by itself, in turn';
    case 'captcha':
      return card.need_you > 1 ? `${card.need_you} need you` : 'Needs you';
    case 'paused':
      return 'Paused';
    // Its reason, on a line of its own, says it failed.
    case 'error':
      return '';
    case 'done': {
      const found = card.found
        ? `${plural(card.found.files, 'file')} from ${plural(card.found.links, 'link')}`
        : 'Unwrapped';
      return card.files ? `${found} · ${partsSummary(card.tally, card.files)}` : found;
    }
  }
}

// One card per set of files, as the engine groups the wrappers: what it holds or
// why it did not open, and what to do about it. One that waits for a person
// is tinted and offers the way through, as a download that needs you does.
function Card({ card, s }: { card: WrapperCard; s: State }) {
  const [pasting, setPasting] = useState(false);
  // Files that wait for a start: never started, or paused.
  const waiting = card.to_start;
  const unstarted = card.tally.not_started;
  const needs = card.status === 'captcha';
  // A card of several wrappers (one per host, or a download page and the
  // pastes it links) counts them, and says what still goes on.
  // A card that found no file has no files to count.
  const state =
    card.ids.length > 1
      ? [
          plural(card.ids.length, 'wrapper'),
          card.files ? plural(card.files, 'file') : '',
          card.files ? partsSummary(card.tally, card.files) : '',
          card.status === 'done' ? '' : status(card),
        ]
          .filter(Boolean)
          .join(' · ')
      : status(card);
  // Pastes that list nothing, beside wrappers that found their files: a
  // quiet note, not a failure.
  const empty = card.empty ? `${plural(card.empty, 'paste lists', 'pastes list')} no links` : '';
  // Links Haul still opens out of sight to learn which file they are: no
  // file of the card until then.
  const checking = card.checking ? `checking ${plural(card.checking, 'link')}` : '';
  const line = [card.host, state, checking, empty].filter(Boolean).join(' · ');
  const reason = card.reason;
  // When a failed one, or the page of one waiting for a person, is tried
  // again on its own, after its reason.
  const again = card.again ? `trying again in ${duration(card.again)}` : '';
  // The wrapper the card shows: the one to open when it needs a person.
  const first = card.open_id;
  const hint = s.desktop
    ? "Open here and pass the page's check: Haul takes what the page then shows (its links, or its .dlc)."
    : 'Open the page, pass its check, then copy the links it shows (or its .dlc link) and paste them here.';
  return (
    <div
      className={cn(
        'flex flex-col gap-3 rounded-xl border px-4 py-3.5',
        needs ? 'border-warn-line bg-warn-soft' : 'bg-card',
      )}
    >
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <Link aria-hidden className={cn('size-5 shrink-0', needs ? 'text-warn' : 'text-muted-foreground')} />
        <div className="min-w-0 flex-[1_1_280px]">
          <div className="truncate text-sm font-medium" title={card.urls.join('\n')}>
            {card.name}
          </div>
          <div className="truncate text-sm text-muted-foreground" title={line}>
            {line}
          </div>
          {/* Why it failed or waits, on a line of its own so it reads whole. */}
          {reason ? (
            <div className="mt-0.5 line-clamp-2 text-sm" title={reason}>
              <span className={card.status === 'error' ? 'text-destructive' : needs ? 'text-warn-strong' : ''}>
                {reason.charAt(0).toUpperCase() + reason.slice(1)}
              </span>
              {again ? <span className="text-muted-foreground"> · {again}</span> : null}
            </div>
          ) : null}
          {/* The hosts its files are on: the same files often have one wrapper per host. */}
          {card.hosts.length ? <Hosts card={card} s={s} /> : null}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {needs ? (
            <NeedsYou
              w={{ id: first, name: card.name, open_urls: card.open_urls, open_hosts: card.open_hosts }}
              desktop={s.desktop}
              paste={() => setPasting(true)}
              hint={hint}
            />
          ) : null}
          {waiting ? (
            <Button variant="outline" onClick={() => startAll([card])}>
              <Play />
              Start all
            </Button>
          ) : null}
          {card.status === 'error' ? (
            <Button variant="ghost" onClick={() => act(first, 'retry')}>
              <RotateCw />
              Retry
            </Button>
          ) : null}
          {/* Its files that never started go with it, so that asks twice. */}
          <ConfirmButton
            label="Remove"
            confirm={unstarted ? `Remove ${plural(unstarted, 'file')} too?` : undefined}
            icon={<X />}
            action={() => removeAll([card])}
          />
        </div>
      </div>
      {pasting && needs ? (
        <div className="pl-9">
          <PasteLink
            w={{ id: first, name: card.name, open_urls: card.open_urls, open_hosts: card.open_hosts }}
            close={() => setPasting(false)}
            done={`Using your link for ${card.name}`}
          />
        </div>
      ) : null}
    </div>
  );
}

export function Wrappers({ s }: { s: State }) {
  const [typed, setTyped] = useState('');
  const cards = s.wrapper_cards;
  const words = typed.trim().toLowerCase();
  const shown = cards.filter((c) => !words || [c.name, c.host, ...c.hosts].join(' ').toLowerCase().includes(words));
  const waiting = cards.filter((c) => c.to_start);
  const failed = cards.filter((c) => c.status === 'error');
  const finished = cards.filter((c) => c.clearable);
  const files = s.counts.wrapper_files_to_start;
  return (
    <>
      <PageHeader
        title="Wrappers"
        actions={
          <>
            {files ? (
              <Button variant="ghost" onClick={() => startAll(waiting)}>
                <Play />
                Start {files}
              </Button>
            ) : null}
            <Button
              variant="ghost"
              disabled={!failed.length}
              onClick={() =>
                wrappersBulk(
                  failed.map((c) => c.open_id),
                  'retry',
                )
              }
            >
              <RotateCw />
              Retry failed
            </Button>
            <Button variant="ghost" disabled={!finished.length} onClick={() => removeAll(finished)}>
              <Eraser />
              Clear completed
            </Button>
            <SearchBox value={typed} label="Search wrappers" onChange={setTyped} />
          </>
        }
      >
        A wrapper link holds a set of files, on one host or many. Haul unwraps it when you add it, and its files wait
        for you to start them.
      </PageHeader>
      {cards.length ? (
        <PageScroll>
          {shown.map((card) => (
            <Card key={card.key} card={card} s={s} />
          ))}
          {shown.length ? null : <p className="text-sm text-muted-foreground">No wrapper matches.</p>}
        </PageScroll>
      ) : (
        <EmptyState
          icon={Link}
          title="No wrapper links yet"
          description="Add a fileguard.cc or filecrypt.cc link: its files join the list, not started."
        />
      )}
    </>
  );
}
