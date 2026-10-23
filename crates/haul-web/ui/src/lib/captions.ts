import type { ArchiveView } from '@/lib/bindings/ArchiveView';
import type { ItemView } from '@/lib/bindings/ItemView';
import type { Tally } from '@/lib/bindings/Tally';
import { bytes, duration } from '@/lib/format';

// A file already in the download folder, being compared with the host's
// checksum.
function diskCaption(it: ItemView, paused: boolean): string {
  if (paused) return 'Waiting to compare with the file on disk (paused)';
  return it.verifying === 'comparing' ? 'Comparing with the file on disk…' : 'Waiting to compare with the file on disk';
}

// What a download is doing, in a few words under its bar.
export function caption(it: ItemView, paused: boolean): string {
  const of = it.size ? `${bytes(it.done)} of ${bytes(it.size)}` : it.done ? bytes(it.done) : '';
  const pct = it.size ? ` · ${Math.floor((it.done / it.size) * 100)}%` : '';
  switch (it.status) {
    case 'downloading':
      return of ? `${of}${pct}` : 'Downloading';
    case 'queued':
      return it.size ? `Queued · ${bytes(it.size)}` : 'Queued';
    case 'checking':
      return it.verifying ? diskCaption(it, paused) : 'Checking the link…';
    case 'resolving':
      return it.verifying ? diskCaption(it, paused) : 'Starting…';
    case 'waiting':
      return it.wait ? `Retrying in ${duration(it.wait)}` : 'Waiting';
    case 'paused':
      if (!it.done) return it.size ? `Not started · ${bytes(it.size)}` : 'Not started';
      return of ? `Paused · ${of}` : 'Paused';
    case 'captcha':
      return 'Needs you';
    case 'passing':
      return 'Haul opens its page by itself, in turn';
    case 'done':
      return `${it.existing ? 'Already downloaded' : 'Completed'}${it.size ? ` · ${bytes(it.size)}` : ''}`;
    case 'error':
      return it.retry_in ? `Failed · retrying in ${duration(it.retry_in)}` : 'Failed';
  }
}

// How far a download is, between 0 and 100.
export function percent(it: ItemView): number {
  if (it.size) return Math.min(100, (it.done / it.size) * 100);
  return it.status === 'done' ? 100 : (it.percent ?? 0);
}

// "12 of 18 done · 3 downloading · 3 failed"
export function partsSummary(t: Tally, total: number): string {
  const out = [`${t.done} of ${total} done`];
  for (const [n, label] of [
    [t.downloading, 'downloading'],
    [t.waiting, 'waiting'],
    [t.not_started, 'not started'],
    [t.paused, 'paused'],
    [t.captcha, 'need you'],
    [t.failed, 'failed'],
  ] as const) {
    if (n) out.push(`${n} ${label}`);
  }
  return out.join(' · ');
}

// The archive's caption: where unpacking it stands once asked for, else the
// parts that are missing, else how the parts stand.
export function archiveCaption(a: ArchiveView): string {
  switch (a.extract) {
    case 'waiting':
      return 'Waiting to extract';
    case 'extracting':
      return `Extracting · ${Math.floor((a.extract_progress ?? 0) * 100)}%`;
    case 'done':
      return 'Extracted';
    case 'failed':
      return `Extraction failed: ${a.extract_error}`;
    case 'password':
      return 'Needs a password';
  }
  const summary = partsSummary(a.tally, a.ids.length);
  if (!a.missing.length) return summary;
  const more = a.missing.length > 1 ? ` and ${a.missing.length - 1} more` : '';
  return `${summary} · ${a.missing[0]}${more} missing`;
}
