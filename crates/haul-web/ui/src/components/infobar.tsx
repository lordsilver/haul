import { CircleCheck, CircleX, Info, TriangleAlert } from 'lucide-react';
import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

export type InfobarKind = 'info' | 'success' | 'caution' | 'critical';

const GLYPHS = { info: Info, success: CircleCheck, caution: TriangleAlert, critical: CircleX };

const KINDS: Record<InfobarKind, string> = {
  info: 'border-border bg-muted/50',
  success: 'border-ok/30 bg-ok-soft',
  caution: 'border-warn-line bg-warn-soft',
  critical: 'border-destructive/30 bg-bad-soft',
};

const GLYPH_COLORS: Record<InfobarKind, string> = {
  info: 'text-muted-foreground',
  success: 'text-ok',
  caution: 'text-warn',
  critical: 'text-destructive',
};

// A bar that tells of a state and offers what to do about it: above a list
// for what affects every download, under a card for that card's own.
export function Infobar({
  kind,
  title,
  children,
  actions,
  className,
}: {
  kind: InfobarKind;
  title?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  const Glyph = GLYPHS[kind];
  return (
    <div
      role={kind === 'critical' ? 'alert' : undefined}
      className={cn(
        'flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border px-3.5 py-2.5 text-sm',
        KINDS[kind],
        className,
      )}
    >
      <Glyph aria-hidden className={cn('size-4 shrink-0', GLYPH_COLORS[kind])} />
      <div className="min-w-0 flex-1">
        {title ? <span className="font-medium">{title} </span> : null}
        <span className="text-muted-foreground">{children}</span>
      </div>
      {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
    </div>
  );
}
