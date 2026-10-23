import { cn } from '@/lib/utils';

export type CountTone = 'plain' | 'info' | 'muted' | 'ok' | 'warn' | 'bad';

const TONES: Record<CountTone, string> = {
  plain: 'text-muted-foreground',
  info: 'bg-info-soft text-info-strong font-medium',
  muted: 'bg-muted text-foreground font-medium',
  ok: 'bg-ok-soft text-ok-strong font-medium',
  warn: 'border-warn-line bg-warn-soft text-warn-strong font-medium',
  bad: 'bg-bad-soft text-bad-strong font-medium',
};

// A count beside a list's name. Every tone sits in the same box, so the
// numbers line up down the sidebar whether a color is behind them or not.
export function Count({ n, tone = 'plain' }: { n: number; tone?: CountTone }) {
  if (!n) return null;
  return (
    <span
      className={cn(
        'ml-auto inline-flex h-5 min-w-5 items-center justify-center rounded-full border border-transparent px-1.5 text-xs tabular-nums',
        TONES[tone],
      )}
    >
      {n}
    </span>
  );
}
