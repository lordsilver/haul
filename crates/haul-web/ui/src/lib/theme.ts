import { useSyncExternalStore } from 'react';
import { api, report } from '@/lib/api';
import type { Theme } from '@/lib/bindings/Theme';
import { prefs } from '@/lib/prefs';

// The theme the user picked; `system` follows the operating system. The
// engine keeps it in the settings file; this browser remembers it too, so
// the first paint (public/theme.js) is already right.
let theme: Theme = (['light', 'dark'] as const).find((t) => t === prefs.get('theme')) ?? 'system';
const listeners = new Set<() => void>();
const dark = matchMedia('(prefers-color-scheme: dark)');

function paint() {
  const isDark = theme === 'dark' || (theme === 'system' && dark.matches);
  document.documentElement.classList.toggle('dark', isDark);
}

dark.addEventListener('change', paint);

// Shows `next`, as the engine reports it or the user picks it.
export function applyTheme(next: Theme) {
  if (next === theme) return;
  theme = next;
  prefs.set('theme', next);
  paint();
  for (const listener of listeners) listener();
}

export function setTheme(next: Theme) {
  applyTheme(next);
  api('/api/settings', { theme: next }).catch(report);
}

export const THEMES: { value: Theme; label: string }[] = [
  { value: 'system', label: 'System' },
  { value: 'light', label: 'Light' },
  { value: 'dark', label: 'Dark' },
];

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useTheme = () => useSyncExternalStore(subscribe, () => theme);

paint();
