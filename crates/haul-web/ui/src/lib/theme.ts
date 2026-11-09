import { type LucideIcon, Monitor, Moon, Sun } from 'lucide-react';
import { report } from '@/lib/api';
import type { Theme } from '@/lib/bindings/Theme';
import { settings } from '@/lib/client';
import { externalStore } from '@/lib/external-store';
import { prefs } from '@/lib/prefs';

// The theme the user picked; `system` follows the operating system. The
// engine keeps it in the settings file; this browser remembers it too, so
// the first paint (public/theme.js) is already right.
let theme: Theme = (['light', 'dark'] as const).find((t) => t === prefs.get('theme')) ?? 'system';
const store = externalStore(() => theme);
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
  store.notify();
}

export function setTheme(next: Theme) {
  applyTheme(next);
  settings.save({ theme: next }).catch(report);
}

export const THEMES = [
  { value: 'system', label: 'System', icon: Monitor },
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
] as const satisfies readonly { value: Theme; label: string; icon: LucideIcon }[];

export const useTheme = store.use;

paint();
