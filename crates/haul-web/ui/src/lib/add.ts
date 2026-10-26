import { useSyncExternalStore } from 'react';
import { addedMessage, api, report } from '@/lib/api';
import type { Added } from '@/lib/bindings/Added';
import { toast } from '@/lib/toast';

// The Add dialog: open or not, and what it holds. Each time it opens it
// starts empty: a package left over from a dropped container would otherwise
// quietly put the next links in its folder.
export type AddForm = { open: boolean; text: string; pkg: string; focus: number };

let form: AddForm = { open: false, text: '', pkg: '', focus: 0 };
const listeners = new Set<() => void>();

function publish(next: AddForm) {
  form = next;
  for (const listener of listeners) listener();
}

// Opens the dialog, or adds `text` to it when it is open already; `pkg`
// fills in the package unless one is typed in.
export function openAdd(text = '', pkg = '') {
  const base = form.open ? form : { open: true, text: '', pkg: '', focus: form.focus };
  const current = base.text.trim();
  publish({
    open: true,
    text: text ? (current ? `${current}\n${text}` : text) : base.text,
    pkg: pkg && !base.pkg.trim() ? pkg : base.pkg,
    focus: base.focus + 1,
  });
}

export const editAdd = (changes: Partial<Pick<AddForm, 'text' | 'pkg'>>) => publish({ ...form, ...changes });
export const closeAdd = () => publish({ ...form, open: false });

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useAddForm = () => useSyncExternalStore(subscribe, () => form);
export const addOpen = () => form.open;

// Adds the links in `text`; answers whether any of them did something.
export async function addText(text: string, pkg = ''): Promise<boolean> {
  if (!text.trim()) return false;
  try {
    const r = await api<Added>('/api/add', { text, package: pkg });
    toast(addedMessage(r), r.added + r.merged + r.wrappers ? 'success' : 'info');
    return r.added + r.merged + r.duplicates + r.wrappers > 0;
  } catch (error) {
    report(error);
    return false;
  }
}

function filesForm(files: FileList | File[]) {
  const body = new FormData();
  for (const file of files) body.append('file', file, file.name);
  return body;
}

// Adds every link in .dlc and text files at once.
export async function importFiles(files: FileList | File[]) {
  if (!files.length) return;
  try {
    toast(addedMessage(await api<Added>('/api/container', filesForm(files))), 'success');
  } catch (error) {
    toast(`Import failed: ${error instanceof Error ? error.message : error}`, 'error');
  }
}

// The links in .dlc and text files go into the Add dialog; nothing is added
// until the user confirms there.
export async function readFiles(files: FileList | File[]) {
  if (!files.length) return;
  try {
    const r = await api<{ links: string[]; package: string }>('/api/container/read', filesForm(files));
    if (!r.links.length) {
      toast('No links in that file', 'info');
      return;
    }
    openAdd(r.links.join('\n'), r.package);
  } catch (error) {
    toast(`Import failed: ${error instanceof Error ? error.message : error}`, 'error');
  }
}
