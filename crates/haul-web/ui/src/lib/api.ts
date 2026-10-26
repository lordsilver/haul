import type { Added } from '@/lib/bindings/Added';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import { toast } from '@/lib/toast';

// Every call is a POST with a JSON body (or a form of files); an error comes
// back as `{"error": "…"}`.
export async function api<T = Record<string, unknown>>(path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method: 'POST' };
  if (body instanceof FormData) {
    init.body = body;
  } else {
    init.headers = { 'Content-Type': 'application/json' };
    init.body = JSON.stringify(body ?? {});
  }
  const res = await fetch(path, init);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `${res.status} ${res.statusText}`);
  return data as T;
}

export const report = (error: unknown) => toast(error instanceof Error ? error.message : String(error), 'error');

const item = (id: string, action: string) => `/api/item/${encodeURIComponent(id)}/${action}`;

export const act = (id: string, action: string) => api(item(id, action)).catch(report);

// The in-app browser window for a page that needs a person.
export const openHere = (id: string) => api(item(id, 'browse'), { user_agent: navigator.userAgent }).catch(report);

export const openPlace = (place: 'downloads' | 'config' | 'data' | 'log') => api(`/api/open/${place}`).catch(report);

// Each action on every item it applies to; one report if any fail. Answers
// how many succeeded.
export async function bulk(ids: string[], action: string): Promise<number> {
  const results = await Promise.allSettled(ids.map((id) => api(item(id, action))));
  const failure = results.find((r) => r.status === 'rejected');
  if (failure) report(failure.reason);
  return results.filter((r) => r.status === 'fulfilled').length;
}

// Every setting, account secrets included: fetched when a page needs it,
// never part of the live state.
export async function fetchSettings(): Promise<SettingsView> {
  const res = await fetch('/api/settings');
  if (!res.ok) throw new Error(`Settings: HTTP ${res.status}`);
  return res.json();
}

// Saves settings; `saved` is the toast on success, null for none. Answers
// whether it saved.
export async function save(changes: Record<string, unknown>, saved: string | null = 'Saved'): Promise<boolean> {
  try {
    await api('/api/settings', changes);
    if (saved) toast(saved, 'success');
    return true;
  } catch (error) {
    report(error);
    return false;
  }
}

export function addedMessage(r: Added): string {
  const parts = r.wrappers && !r.added ? [] : [`${r.added} added`];
  if (r.wrappers) parts.push(`${r.wrappers} ${r.wrappers === 1 ? 'wrapper' : 'wrappers'} to unwrap`);
  if (r.merged) parts.push(`${r.merged} merged as mirrors`);
  if (r.duplicates) parts.push(`${r.duplicates} already in the list`);
  if (r.invalid) parts.push(`${r.invalid} not http(s)`);
  return parts.join(' · ');
}
