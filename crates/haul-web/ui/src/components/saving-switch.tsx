import { useEffect, useState } from 'react';
import { Switch } from '@/components/ui/switch';

// A switch that flips at once and saves; it flips back if the save fails.
// `change` answers whether it saved, and says why itself when it did not.
export function SavingSwitch({
  on,
  label,
  disabled,
  change,
  onShown,
}: {
  on: boolean;
  label: string;
  disabled?: boolean;
  change: (on: boolean) => Promise<boolean>;
  // What it shows now, for a word beside it.
  onShown?: (on: boolean) => void;
}) {
  const [value, setValue] = useState(on);
  const [saving, setSaving] = useState(false);
  useEffect(() => setValue(on), [on]);
  useEffect(() => onShown?.(value), [value, onShown]);
  return (
    <Switch
      checked={value}
      disabled={disabled || saving}
      aria-label={label}
      onCheckedChange={async (next) => {
        setValue(next);
        setSaving(true);
        if (!(await change(next))) setValue(!next);
        setSaving(false);
      }}
    />
  );
}
