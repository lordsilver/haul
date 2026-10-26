import { AppWindow, ChevronDown, ExternalLink, Link, RotateCw } from 'lucide-react';
import { type FormEvent, useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Input } from '@/components/ui/input';
import { act, api, openHere, report } from '@/lib/api';
import { hostOf, isWebLink } from '@/lib/format';
import { toast } from '@/lib/toast';

// What waits for a person: a download or a wrapper, with the pages it can be
// solved on.
export type Waiting = { id: string; name: string; open_urls: string[] };

// The pages a download can be solved on, in the system's browser: one item
// for one page, else a submenu of their hosts.
function BrowserPages({ pages, label }: { pages: string[]; label: string }) {
  if (pages.length === 1 && pages[0]) {
    return (
      <DropdownMenuItem asChild title={pages[0]}>
        <a href={pages[0]} target="_blank" rel="noopener noreferrer">
          <ExternalLink />
          {label}
        </a>
      </DropdownMenuItem>
    );
  }
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <ExternalLink />
        {label}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent>
        {pages.map((url) => (
          <DropdownMenuItem key={url} asChild title={url}>
            <a href={url} target="_blank" rel="noopener noreferrer">
              {hostOf(url)}
            </a>
          </DropdownMenuItem>
        ))}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}

// The way through for something that waits for a person: one button for the
// usual way (the app's own browser on the desktop, the host's page in a
// browser tab), the others in its menu. `paste` opens the field for a link
// the page handed over.
export function NeedsYou({
  w,
  desktop,
  paste,
  hint,
}: {
  w: Waiting;
  desktop: boolean;
  paste: () => void;
  hint: string;
}) {
  const pages = w.open_urls.filter(isWebLink);
  const first = pages[0];
  const primary = desktop ? (
    <Button size="sm" className="rounded-r-none" title={hint} onClick={() => openHere(w.id)}>
      <AppWindow />
      Open here
    </Button>
  ) : first && pages.length === 1 ? (
    <Button size="sm" className="rounded-r-none" title={hint} asChild>
      <a href={first} target="_blank" rel="noopener noreferrer">
        <ExternalLink />
        Open page
      </a>
    </Button>
  ) : (
    <Button size="sm" className="rounded-r-none" title={hint} onClick={paste}>
      <Link />
      Paste link
    </Button>
  );
  return (
    <div className="inline-flex">
      {primary}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            size="icon-sm"
            className="w-7 rounded-l-none border-l border-l-primary-foreground/25"
            aria-label="Other ways to get this download"
            title="Other ways to get this download"
          >
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-72">
          {pages.length ? <BrowserPages pages={pages} label={desktop ? 'Open in browser' : 'Open page'} /> : null}
          <DropdownMenuItem onSelect={paste}>
            <Link />
            Paste link
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => act(w.id, 'retry_manual')}>
            <RotateCw />
            Try again
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <p className="px-2 py-1.5 text-xs text-muted-foreground">{hint}</p>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

// The link a host's page handed over, typed or pasted in: Haul downloads it
// for this row instead of resolving the page again.
export function PasteLink({ w, close, done }: { w: Waiting; close: () => void; done: string }) {
  const input = useRef<HTMLInputElement>(null);
  const [url, setUrl] = useState('');
  useEffect(() => input.current?.focus(), []);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api(`/api/item/${encodeURIComponent(w.id)}/direct`, { url: url.trim() });
      close();
      toast(done, 'success');
    } catch (error) {
      report(error);
    }
  };
  return (
    <form className="flex flex-wrap items-center gap-2" onSubmit={submit}>
      <Input
        ref={input}
        type="url"
        required
        value={url}
        placeholder="https://… the direct download link"
        aria-label="Direct download link"
        className="max-w-md flex-1"
        onChange={(e) => setUrl(e.target.value)}
        onKeyDown={(e) => e.key === 'Escape' && close()}
      />
      <Button type="submit" size="sm">
        Use link
      </Button>
      <Button type="button" size="sm" variant="ghost" onClick={close}>
        Cancel
      </Button>
    </form>
  );
}
