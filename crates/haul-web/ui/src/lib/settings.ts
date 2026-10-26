import { useEffect, useState } from 'react';
import { fetchSettings, report } from '@/lib/api';
import type { SettingsView } from '@/lib/bindings/SettingsView';

// Every setting, fetched again whenever the engine says one changed
// (`config_version`).
export function useSettings(version: number | undefined): SettingsView | null {
  const [settings, setSettings] = useState<SettingsView | null>(null);
  useEffect(() => {
    if (version === undefined) return;
    let current = true;
    fetchSettings().then((s) => current && setSettings(s), report);
    return () => {
      current = false;
    };
  }, [version]);
  return settings;
}

// Why a key cannot change here, when an environment variable sets it.
export function envNote(d: SettingsView, ...keys: string[]): string | null {
  const variable = keys.map((k) => d.from_env[k]).find(Boolean);
  return variable ? `Set by the ${variable} environment variable.` : null;
}
