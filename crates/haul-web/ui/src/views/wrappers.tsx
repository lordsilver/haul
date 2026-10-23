import { Link, Play, RotateCw, X } from 'lucide-react';
import { useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { Button } from '@/components/ui/button';
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty';
import { act, api, report } from '@/lib/api';
import type { State } from '@/lib/bindings/State';
import type { WrapperCard } from '@/lib/bindings/WrapperCard';
import { partsSummary } from '@/lib/captions';
import { duration, plural } from '@/lib/format';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { NeedsYou, PasteLink } from '@/views/downloads/needs-you';

// A card's wrappers together: their files counted once, since a file two of
// them list starts with the first.
function startAll(card: WrapperCard) {
  return Promise.all(
    card.ids.map((id) => api<{ started?: number }>(`/api/wrapper/${encodeURIComponent(id)}/start`)),
  ).then((rs) => {
    const started = rs.reduce((n, r) => n + (r.started || 0), 0);
    if (started) toast(`Started ${plural(started, 'file')} from ${card.name}`, 'success');
  }, report);
}

const removeAll = (card: WrapperCard) =>
  Promise.all(card.ids.map((id) => api(`/api/wrapper/${encodeURIComponent(id)}/remove`)));

// What a wrapper is doing, in a few words; once unwrapped, how its files
// stand.
function status(card: WrapperCard): string {
  switch (card.status) {
    case 'unwrapping':
      return 'Unwrapping…';
    case 'waiting':
      return card.wait ? `Trying again in ${duration(card.wait)}` : 'Waiting to try again';
    case 'captcha':
      return 'Needs you';
    case 'paused':
      return 'Paused';
    case 'error':
      return card.retry_in ? `Failed · trying again in ${duration(card.retry_in)}` : 'Failed';
    case 'done': {
      const found = card.found
        ? `${plural(card.found.files, 'file')} from ${plural(card.found.links, 'link')}`
        : 'Unwrapped';
      return card.files ? `${found} · ${partsSummary(card.tally, card.files)}` : found;
    }
  }
}

// One card per release, as the engine groups the wrappers: what it holds or
// why it did not open, and what to do about it. One that waits for a person
// is tinted and offers the way through, as a download that needs you does.
function Card({ card, s }: { card: WrapperCard; s: State }) {
  const [pasting, setPasting] = useState(false);
  // Files that wait for a start: never started, or paused.
  const waiting = card.tally.not_started + card.tally.paused;
  const unstarted = card.tally.not_started;
  const needs = card.status === 'captcha';
  // The hosts the links are on, over every wrapper of the card: a release
  // often has one wrapper per host.
  const on = card.hosts.length
    ? `${card.hosts.slice(0, 3).join(', ')}${card.hosts.length > 3 ? ` +${card.hosts.length - 3}` : ''}`
    : '';
  const state =
    card.ids.length > 1
      ? `${plural(card.ids.length, 'wrapper')} · ${plural(card.files, 'file')} · ${partsSummary(card.tally, card.files)}`
      : status(card);
  const line = [card.host, on, state].filter(Boolean).join(' · ');
  const reason = ['error', 'waiting', 'captcha'].includes(card.status) ? card.error_shown : '';
  const first = card.ids[0] ?? '';
  const hint = s.desktop
    ? "Open here, pass the page's check and press its .dlc button: Haul unwraps what the page hands over."
    : 'Open the page, pass its check, then copy the .dlc link it offers and paste it here.';
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
          <div className="truncate text-sm text-muted-foreground" title={card.hosts.join(', ')}>
            {line}
            {reason ? (
              <span className={card.status === 'error' ? 'text-destructive' : needs ? 'text-warn-strong' : ''}>
                {' '}
                · {reason}
              </span>
            ) : null}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {needs ? (
            <NeedsYou
              w={{ id: first, name: card.name, open_urls: card.open_urls }}
              desktop={s.desktop}
              paste={() => setPasting(true)}
              hint={hint}
            />
          ) : null}
          {waiting ? (
            <Button onClick={() => startAll(card)}>
              <Play />
              Start all
            </Button>
          ) : null}
          {card.status === 'error' ? (
            <Button variant="ghost" onClick={() => act(first, 'retry')}>
              <RotateCw />
              Try again
            </Button>
          ) : null}
          {/* Its files that never started go with it, so that asks twice. */}
          {unstarted ? (
            <ConfirmButton
              label="Remove"
              confirm={`Remove ${plural(unstarted, 'file')} too?`}
              icon={<X />}
              action={() => removeAll(card)}
            />
          ) : (
            <Button variant="ghost" onClick={() => removeAll(card).catch(report)}>
              <X />
              Remove
            </Button>
          )}
        </div>
      </div>
      {pasting && needs ? (
        <div className="pl-9">
          <PasteLink
            w={{ id: first, name: card.name, open_urls: card.open_urls }}
            close={() => setPasting(false)}
            done={`Using your link for ${card.name}`}
          />
        </div>
      ) : null}
    </div>
  );
}

export function Wrappers({ s }: { s: State }) {
  return (
    <>
      <header className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Wrappers</h1>
        <p className="max-w-3xl text-sm text-muted-foreground">
          A wrapper link, such as fileguard.cc's, filecrypt.cc's or mirrored.to's, holds a release's files, on one host
          or on many. Haul unwraps it as soon as you add it, and each file joins the list once: the same file from
          another host, on this wrapper or another one, becomes a mirror of it rather than a second download. The files
          wait for you to start them, here or in the list.
        </p>
      </header>
      {s.wrapper_cards.length ? (
        <div className="-mx-2 min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-2 pb-4">
          <div className="flex max-w-4xl flex-col gap-2.5">
            {s.wrapper_cards.map((card) => (
              <Card key={card.key} card={card} s={s} />
            ))}
          </div>
        </div>
      ) : (
        <Empty className="flex-1 border">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <Link />
            </EmptyMedia>
            <EmptyTitle>No wrapper links yet</EmptyTitle>
            <EmptyDescription>
              Add a fileguard.cc or filecrypt.cc link: its files join the list, not started.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </>
  );
}
