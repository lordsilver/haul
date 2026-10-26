import type { LucideIcon } from 'lucide-react';
import { type ReactNode, useEffect, useState } from 'react';
import { SecretInput } from '@/components/secret-input';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { save } from '@/lib/api';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';

// One setting, or one thing: its icon, name and what it does, its controls
// on the right, and below them whatever belongs to it (a bar, a form).
export function SettingCard({
  icon: Icon,
  title,
  description,
  controls,
  children,
  className,
  ...props
}: {
  icon: LucideIcon;
  title: ReactNode;
  description?: ReactNode;
  controls?: ReactNode;
  children?: ReactNode;
  className?: string;
} & React.ComponentProps<'div'>) {
  return (
    <div className={cn('flex flex-col gap-3 rounded-xl border bg-card px-4 py-3.5', className)} {...props}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <Icon aria-hidden className="size-5 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-[1_1_280px]">
          <div className="text-sm font-medium">{title}</div>
          {description ? <div className="mt-0.5 text-sm text-muted-foreground">{description}</div> : null}
        </div>
        {controls ? <div className="flex flex-wrap items-center justify-end gap-2">{controls}</div> : null}
      </div>
      {children}
    </div>
  );
}

// A part of a card's setting that belongs to it: below a line, under the
// card's text, with its own name, words and controls.
export function SettingRow({
  title,
  description,
  controls,
  children,
  disabled = false,
}: {
  title: ReactNode;
  description?: ReactNode;
  controls?: ReactNode;
  children?: ReactNode;
  disabled?: boolean;
}) {
  return (
    <div className={cn('flex flex-col gap-2 border-t pt-3 pl-9', disabled && 'opacity-60')}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <div className="min-w-0 flex-[1_1_280px] text-sm">
          <div className="font-medium">{title}</div>
          {description ? <div className="mt-0.5 text-muted-foreground">{description}</div> : null}
        </div>
        {controls ? <div className="flex flex-wrap items-center justify-end gap-2">{controls}</div> : null}
      </div>
      {children}
    </div>
  );
}

// A heading over a group of cards.
export const Section = ({ children }: { children: ReactNode }) => (
  <h2 className="mt-4 mb-0.5 text-sm font-semibold first:mt-0">{children}</h2>
);

// A switch that saves its setting at once.
export function SettingSwitch({
  name,
  on,
  label,
  disabled = false,
}: {
  name: string;
  on: boolean;
  label: string;
  disabled?: boolean;
}) {
  return (
    <span className="flex items-center gap-2 text-sm text-muted-foreground">
      {on ? 'On' : 'Off'}
      <Switch
        checked={on}
        disabled={disabled}
        aria-label={label}
        onCheckedChange={(next) => save({ [name]: next }, null)}
      />
    </span>
  );
}

// A number saved when it changes, within its bounds.
export function NumberField({
  name,
  value,
  min,
  max,
  step = 1,
  label,
  unit,
}: {
  name: string;
  value: number;
  min: number;
  max?: number;
  step?: number;
  label: string;
  unit?: string;
}) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  const commit = () => {
    const n = Number(text);
    if (text.trim() === '' || !Number.isFinite(n) || n < min || (max !== undefined && n > max)) {
      setText(String(value));
      toast(max === undefined ? `${label}: ${min} or more` : `${label}: from ${min} to ${max}`, 'error');
      return;
    }
    if (n !== value) save({ [name]: n });
  };
  return (
    <span className="flex items-center gap-2">
      <Input
        type="number"
        min={min}
        max={max}
        step={step}
        value={text}
        aria-label={label}
        className="w-24"
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
      />
      {unit ? <span className="text-sm text-muted-foreground">{unit}</span> : null}
    </span>
  );
}

// A line of text saved when it changes; a secret one hides what it holds.
export function TextField({
  name,
  value,
  d,
  label,
  placeholder = '',
  secret = false,
  wide = false,
}: {
  name: string;
  value: string;
  d: SettingsView;
  label: string;
  placeholder?: string;
  secret?: boolean;
  wide?: boolean;
}) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const props = {
    value: text,
    placeholder,
    'aria-label': label,
    autoComplete: 'off',
    spellCheck: false,
    disabled: Boolean(d.from_env[name]),
    className: wide ? 'w-full sm:w-96' : 'w-56',
    onChange: (e: React.ChangeEvent<HTMLInputElement>) => setText(e.target.value),
    onBlur: () => text.trim() !== value && save({ [name]: text.trim() }),
    onKeyDown: (e: React.KeyboardEvent<HTMLInputElement>) => e.key === 'Enter' && (e.target as HTMLInputElement).blur(),
  };
  return secret ? <SecretInput {...props} /> : <Input {...props} />;
}

// An account key from the settings, by its name in the file.
export const AccountField = ({
  d,
  name,
  label,
  secret = true,
}: {
  d: SettingsView;
  name: string;
  label: string;
  secret?: boolean;
}) => <TextField d={d} name={name} value={d.accounts[name] ?? ''} label={label} placeholder={label} secret={secret} />;

// Lines of text, one entry per line, saved when the box loses focus.
export function LinesField({
  name,
  lines,
  label,
  placeholder,
}: {
  name: string;
  lines: string[];
  label: string;
  placeholder?: string;
}) {
  const joined = lines.join('\n');
  const [text, setText] = useState(joined);
  useEffect(() => setText(joined), [joined]);
  return (
    <Textarea
      rows={Math.min(8, Math.max(3, lines.length + 1))}
      spellCheck={false}
      autoComplete="off"
      aria-label={label}
      placeholder={placeholder}
      className="font-mono text-xs"
      value={text}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== joined && save({ [name]: text })}
    />
  );
}
