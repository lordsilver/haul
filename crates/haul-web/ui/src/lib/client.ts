import type { Added } from '@/lib/bindings/Added';
import type { Batch } from '@/lib/bindings/Batch';
import type { SettingsView } from '@/lib/bindings/SettingsView';

// Every route of Haul's API the page uses, one function each: no path is
// written anywhere else. Each answers what the route answers and throws
// with its reason when it fails, for the caller to report.

// Every call is a POST with a JSON body (or a form of files); an error comes
// back as `{"error": "…"}`.
async function api<T = Record<string, unknown>>(path: string, body?: unknown): Promise<T> {
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

const e = encodeURIComponent;

// What GETs and the event stream are read from.
export const paths = {
  events: '/api/events',
  settings: '/api/settings',
  diagnostics: '/api/diagnostics',
};

export const items = {
  act: (ids: string[], action: string) => api<Batch>(`/api/items/${action}`, { ids }),
};

export const item = {
  act: (id: string, action: string) => api(`/api/item/${e(id)}/${action}`),
  // Downloads it from a link the person supplies.
  direct: (id: string, url: string) => api(`/api/item/${e(id)}/direct`, { url }),
  // Its page in the desktop app's own window.
  browse: (id: string) => api(`/api/item/${e(id)}/browse`, { user_agent: navigator.userAgent }),
  extract: (id: string, password?: string) =>
    api(`/api/item/${e(id)}/extract`, password === undefined ? {} : { password }),
};

export const wrappers = {
  act: (ids: string[], action: 'start' | 'remove' | 'retry') => api<Batch>(`/api/wrappers/${action}`, { ids }),
  dropHost: (ids: string[], host: string) => api<{ dropped: number }>('/api/wrappers/drop-host', { ids, host }),
};

export const hosts = {
  folder: (name: string, folder: string) => api(`/api/host/${e(name)}/folder`, { folder }),
  preferred: (name: string, preferred: boolean) => api(`/api/host/${e(name)}/preferred`, { preferred }),
  disabled: (name: string, disabled: boolean) => api(`/api/host/${e(name)}/disabled`, { disabled }),
  accountless: (name: string, accountless: boolean) => api(`/api/host/${e(name)}/accountless`, { accountless }),
  route: (name: string, route: 'proxy' | 'direct') => api(`/api/host/${e(name)}/route/${route}`),
  preferredOrder: (names: string[]) => api('/api/preferred-hosts', { hosts: names }),
  // Downloads go to `host` while it has their file; `null`: the usual order.
  downloadFrom: (ids: string[], host: string | null) => api('/api/download-from', { ids, host }),
};

export const queue = (action: 'pause' | 'resume' | 'retry_failed' | 'clear_done' | 'clear_failed') =>
  api(`/api/global/${action}`);

export const adding = {
  text: (text: string, pkg: string, password: string) => api<Added>('/api/add', { text, package: pkg, password }),
  files: (body: FormData) => api<Added>('/api/container', body),
  read: (body: FormData) => api<{ links: string[]; package: string }>('/api/container/read', body),
  // A paste outside a field: the engine says whether it is a link for the
  // one download that waits for one, or text for the Add dialog.
  paste: (text: string) => api<{ used_for?: string; name?: string; open_add?: string }>('/api/paste', { text }),
};

// Every setting, account secrets included: fetched when a page needs it,
// never part of the live state.
export async function fetchSettings(): Promise<SettingsView> {
  const res = await fetch(paths.settings);
  if (!res.ok) throw new Error(`Settings: HTTP ${res.status}`);
  return res.json();
}

export const settings = {
  save: (changes: Record<string, unknown>) => api(paths.settings, changes),
  pickDownloadDir: () => api<{ cancelled?: boolean }>('/api/settings/download-dir/pick'),
};

export const connections = {
  reconnect: () => api<{ address: string }>('/api/reconnect'),
  findRouter: () => api('/api/router'),
  nordvpnServers: () => api('/api/nordvpn/servers'),
};

export const update = {
  check: () => api<{ version: string | null }>('/api/update/check'),
  install: () => api('/api/update/install'),
};

export const diagnostics = {
  save: () => api<{ saved?: string }>('/api/diagnostics/save'),
};

export const open = (place: 'downloads' | 'config' | 'data') => api(`/api/open/${place}`);
