import { cn } from '@/lib/utils';

// How much of a name's end always shows: enough for `.part12.rar`.
const TAIL = 12;

// A file's name on one line. One too long for its column loses its middle,
// not its end, so the parts of one archive (`….part1.rar`, `….part2.rar`)
// still read apart.
export function FileName({ name, className }: { name: string; className?: string }) {
  const cut = name.length > TAIL * 2 ? name.length - TAIL : name.length;
  return (
    <div className={cn('flex min-w-0 font-medium', className)} title={name}>
      <span className="truncate">{name.slice(0, cut)}</span>
      {cut < name.length ? <span className="shrink-0 whitespace-pre">{name.slice(cut)}</span> : null}
    </div>
  );
}
