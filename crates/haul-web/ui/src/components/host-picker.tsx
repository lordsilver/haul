import { ChevronDown, Pin, Server } from 'lucide-react';
import type { ReactNode } from 'react';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { api, report } from '@/lib/api';

const AUTOMATIC = '';

// A host's name as a row shows it: pinned when the user picked it for the
// file, starred when it is one of their preferred hosts.
export function hostLabel(text: string, { picked, preferred }: { picked: boolean; preferred: boolean }): ReactNode {
  if (picked) {
    return (
      <>
        <Pin aria-label="Picked for this file" className="inline size-3 align-[-2px]" /> {text}
      </>
    );
  }
  return preferred ? `★ ${text}` : text;
}

const MIXED = '\u0000mixed';

// Where a file comes from, and on a file several hosts have, the choice of
// which: Automatic (the preferred hosts, else the fastest) or one of them,
// the others standing by. For an archive, `ids` are its parts; for rows
// picked together (`bar`), each takes the host if it has it.
export function HostPicker({
  ids,
  hosts,
  chosen,
  label,
  bar = false,
}: {
  ids: string[];
  hosts: string[];
  chosen: string | null | undefined;
  label: ReactNode;
  bar?: boolean;
}) {
  if (hosts.length < 2 && !bar) return <span>{label}</span>;
  const pick = (host: string) =>
    api('/api/download-from', { ids, host: host === AUTOMATIC ? null : host }).catch(report);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        {bar ? (
          <Button variant="ghost">
            <Server />
            {label}
          </Button>
        ) : (
          <button
            type="button"
            title="Choose the host to download from"
            className="inline-flex items-center gap-0.5 rounded-sm underline-offset-2 hover:text-foreground hover:underline"
          >
            {label}
            <ChevronDown aria-hidden className="size-3" />
          </button>
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-56">
        <DropdownMenuLabel>Download from</DropdownMenuLabel>
        {/* Picked rows that differ have no choice in common: none is ticked. */}
        <DropdownMenuRadioGroup value={chosen === undefined ? MIXED : (chosen ?? AUTOMATIC)} onValueChange={pick}>
          <DropdownMenuRadioItem value={AUTOMATIC}>Automatic</DropdownMenuRadioItem>
          <DropdownMenuSeparator />
          {hosts.map((h) => (
            <DropdownMenuRadioItem key={h} value={h}>
              {h}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
