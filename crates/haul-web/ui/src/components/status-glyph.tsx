import {
  Archive,
  CircleArrowDown,
  CircleCheck,
  CirclePause,
  CircleX,
  Clock,
  type LucideIcon,
  TriangleAlert,
} from 'lucide-react';
import type { ItemView } from '@/lib/bindings/ItemView';
import { cn } from '@/lib/utils';

type Status = ItemView['status'];

const GLYPHS: Partial<Record<Status, LucideIcon>> = {
  downloading: CircleArrowDown,
  paused: CirclePause,
  captcha: TriangleAlert,
  done: CircleCheck,
  error: CircleX,
};

// The color of a state, wherever it shows.
export const STATUS_COLORS: Partial<Record<Status, string>> = {
  downloading: 'text-info',
  captcha: 'text-warn',
  done: 'text-ok',
  error: 'text-destructive',
};

export function StatusGlyph({ status, archive = false }: { status: Status; archive?: boolean }) {
  const Glyph = archive ? Archive : (GLYPHS[status] ?? Clock);
  return <Glyph aria-hidden className={cn('size-4 shrink-0', STATUS_COLORS[status] ?? 'text-muted-foreground')} />;
}
