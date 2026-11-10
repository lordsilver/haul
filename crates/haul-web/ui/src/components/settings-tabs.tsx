import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { prefs } from '@/lib/prefs';
import { go } from '@/lib/route';

// The groups Settings is split into, in order. Hosts and Connections are
// pages of their own, shown as two of its tabs.
export const SETTINGS_TABS = [
  { value: 'general', label: 'General' },
  { value: 'downloads', label: 'Downloads' },
  { value: 'archives', label: 'Archives' },
  { value: 'hosts', label: 'Hosts' },
  { value: 'connections', label: 'Connections' },
  { value: 'about', label: 'About' },
] as const;
export type SettingsTab = (typeof SETTINGS_TABS)[number]['value'];
export type OwnTab = Exclude<SettingsTab, 'hosts' | 'connections'>;

// The tab of the Settings page itself last open, kept in this browser.
export function savedTab(): OwnTab {
  const saved = prefs.get('settings.tab');
  const tab = SETTINGS_TABS.find((t) => t.value === saved)?.value;
  return tab && tab !== 'hosts' && tab !== 'connections' ? tab : 'general';
}

// Tells an open Settings page to show another tab.
const PICKED = 'haul:settings-tab';

// Opens Settings on `tab`, from anywhere, the page open or not.
export function openSettings(tab: OwnTab) {
  prefs.set('settings.tab', tab);
  dispatchEvent(new CustomEvent<OwnTab>(PICKED, { detail: tab }));
  go('settings');
}

// The tab Settings shows: the last one open, or the one `openSettings` asks for.
export function useSettingsTab() {
  const [tab, setTab] = useState<OwnTab>(savedTab);
  useEffect(() => {
    const picked = (e: Event) => setTab((e as CustomEvent<OwnTab>).detail);
    addEventListener(PICKED, picked);
    return () => removeEventListener(PICKED, picked);
  }, []);
  return [tab, setTab] as const;
}

// The row of tabs after the Settings title, on each of its pages.
export function SettingsTabs({ current, pick }: { current: SettingsTab; pick?: (tab: OwnTab) => void }) {
  const open = (tab: SettingsTab) => {
    if (tab === 'hosts' || tab === 'connections') {
      go(tab);
      return;
    }
    prefs.set('settings.tab', tab);
    if (pick) pick(tab);
    else go('settings');
  };
  return (
    <div role="tablist" aria-label="Settings" className="flex flex-wrap gap-1">
      {SETTINGS_TABS.map((t) => (
        <Button
          key={t.value}
          role="tab"
          aria-selected={current === t.value}
          variant={current === t.value ? 'secondary' : 'ghost'}
          onClick={() => open(t.value)}
        >
          {t.label}
        </Button>
      ))}
    </div>
  );
}
