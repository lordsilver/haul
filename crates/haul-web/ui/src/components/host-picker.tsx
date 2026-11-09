import { ChevronDown, Pin, Server } from 'lucide-react';
import { Fragment, type ReactNode } from 'react';
import { Hint } from '@/components/hint';
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
import { report } from '@/lib/api';
import type { State } from '@/lib/bindings/State';
import { hosts as hostRoutes } from '@/lib/client';

const AUTOMATIC = '';

// The hosts that are off: switched off, or off until their account is set
// or they are switched on in Hosts. Rows grey them out.
export function offHosts(s: State): Set<string> {
  return new Set(s.hosts.filter((h) => h.disabled).map((h) => h.name));
}

// A host's name as a row lists it: greyed out while it is off.
export function HostName({ host, off }: { host: string; off: boolean }) {
  if (!off) return <>{host}</>;
  return (
    <Hint text={`${host} is off (Hosts)`}>
      <span className="text-muted-foreground/70">{host}</span>
    </Hint>
  );
}

// Hosts as a row lists them, one after the other.
export function HostNames({ hosts, off }: { hosts: string[]; off: Set<string> }) {
  return (
    <>
      {hosts.map((h, i) => (
        <Fragment key={h}>
          {i ? ', ' : null}
          <HostName host={h} off={off.has(h)} />
        </Fragment>
      ))}
    </>
  );
}

// A host's name as a row shows it: pinned when the user picked it for the
// file, starred when it is one of their preferred hosts.
export function hostLabel(text: ReactNode, { picked, preferred }: { picked: boolean; preferred: boolean }): ReactNode {
  if (picked) {
    return (
      <>
        <Pin aria-label="Picked for this file" className="inline size-3 align-[-2px]" /> {text}
      </>
    );
  }
  return preferred ? <>★ {text}</> : text;
}

const MIXED = '\u0000mixed';

// Where a file comes from, and on a file several hosts have, the choice of
// which: Automatic (the preferred hosts, else the fastest) or one of them,
// the others standing by. For an archive, `ids` are its parts; for rows
// picked together (`bar`), each takes the host if it has it. `off` are the
// hosts that are off (offHosts).
export function HostPicker({
  ids,
  hosts,
  off,
  chosen,
  label,
  bar = false,
}: {
  ids: string[];
  hosts: string[];
  off: Set<string>;
  chosen: string | null | undefined;
  label: ReactNode;
  bar?: boolean;
}) {
  if (hosts.length < 2 && !bar) return <span>{label}</span>;
  const pick = (host: string) => hostRoutes.downloadFrom(ids, host === AUTOMATIC ? null : host).catch(report);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        {bar ? (
          <Button variant="ghost">
            <Server />
            {label}
          </Button>
        ) : (
          <Hint text="Choose the host to download from">
            <button
              type="button"
              className="inline-flex items-center gap-0.5 rounded-sm underline-offset-2 hover:text-foreground hover:underline"
            >
              {label}
              <ChevronDown aria-hidden className="size-3" />
            </button>
          </Hint>
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
              <HostName host={h} off={off.has(h)} />
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
