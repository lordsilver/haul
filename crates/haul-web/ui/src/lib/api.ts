import type { Added } from '@/lib/bindings/Added';
import type { Batch } from '@/lib/bindings/Batch';
import * as client from '@/lib/client';
import { plural } from '@/lib/format';
import { toast } from '@/lib/toast';

export const report = (error: unknown) => toast(error instanceof Error ? error.message : String(error), 'error');

export const act = (id: string, action: string) => client.item.act(id, action).catch(report);

// The in-app browser window for a page that needs a person.
export const openHere = (id: string) => client.item.browse(id).catch(report);

export const openPlace = (place: 'downloads' | 'config' | 'data') => client.open(place).catch(report);

// One action on many items in one request; the first failure, and how many
// others there were, is reported. Answers how many succeeded.
export async function bulk(ids: string[], action: string): Promise<number> {
  if (!ids.length) return 0;
  try {
    const r = await client.items.act(ids, action);
    reportFailed(r);
    return r.done;
  } catch (error) {
    report(error);
    return 0;
  }
}

// One action on many wrappers (a card's, or every card's) in one request.
export async function wrappersBulk(ids: string[], action: 'start' | 'remove' | 'retry'): Promise<Batch | null> {
  if (!ids.length) return null;
  try {
    const r = await client.wrappers.act(ids, action);
    reportFailed(r);
    return r;
  } catch (error) {
    report(error);
    return null;
  }
}

function reportFailed(r: Batch) {
  const [first, ...rest] = r.failed;
  if (!first) return;
  toast(rest.length ? `${first.error} (and ${plural(rest.length, 'other')})` : first.error, 'error');
}

export const fetchSettings = client.fetchSettings;

// Saves settings; a failure always says why. A control that shows its new
// value needs no word for success: `saved` is a toast only for a result
// that is not seen in place. Answers whether it saved.
export async function save(changes: Record<string, unknown>, saved: string | null = null): Promise<boolean> {
  try {
    await client.settings.save(changes);
    if (saved) toast(saved, 'success');
    return true;
  } catch (error) {
    report(error);
    return false;
  }
}

export function addedMessage(r: Added): string {
  const parts = r.wrappers && !r.added ? [] : [`${r.added} added`];
  if (r.wrappers) parts.push(`${plural(r.wrappers, 'wrapper')} to unwrap`);
  if (r.merged) parts.push(`${r.merged} merged as mirrors`);
  if (r.reread) parts.push(`${r.reread === 1 ? 'its card is' : `${r.reread} cards are`} read again`);
  if (r.duplicates) parts.push(`${r.duplicates} already in the list`);
  if (r.invalid) parts.push(`${r.invalid} not http(s)`);
  return parts.join(' · ');
}
