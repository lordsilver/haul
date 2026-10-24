import { ChevronDown } from 'lucide-react';
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

// Where a file comes from, and on a file several hosts have, the choice of
// which: Automatic (the preferred hosts, else the fastest) or one of them,
// the others standing by. For an archive, `ids` are its parts.
export function HostPicker({
  ids,
  hosts,
  chosen,
  label,
}: {
  ids: string[];
  hosts: string[];
  chosen: string | null;
  label: string;
}) {
  if (hosts.length < 2) return <span>{label}</span>;
  const pick = (host: string) =>
    api('/api/download-from', { ids, host: host === AUTOMATIC ? null : host }).catch(report);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          title="Choose the host to download from"
          className="inline-flex items-center gap-0.5 rounded-sm underline-offset-2 hover:text-foreground hover:underline"
        >
          {label}
          <ChevronDown aria-hidden className="size-3" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-56">
        <DropdownMenuLabel>Download from</DropdownMenuLabel>
        <DropdownMenuRadioGroup value={chosen ?? AUTOMATIC} onValueChange={pick}>
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
