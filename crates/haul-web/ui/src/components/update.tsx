import { ArrowUpCircle, ExternalLink, LoaderCircle } from 'lucide-react';
import { useState } from 'react';
import Markdown, { type Components } from 'react-markdown';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { report } from '@/lib/api';
import type { State } from '@/lib/bindings/State';
import { update } from '@/lib/client';

// Release notes, in the dialog's own type.
const NOTES: Components = {
  h1: ({ children }) => <h3 className="mt-3 mb-1 font-semibold first:mt-0">{children}</h3>,
  h2: ({ children }) => <h3 className="mt-3 mb-1 font-semibold first:mt-0">{children}</h3>,
  h3: ({ children }) => <h3 className="mt-3 mb-1 font-semibold first:mt-0">{children}</h3>,
  p: ({ children }) => <p className="my-1.5">{children}</p>,
  ul: ({ children }) => <ul className="my-1.5 list-disc space-y-1 pl-5">{children}</ul>,
  ol: ({ children }) => <ol className="my-1.5 list-decimal space-y-1 pl-5">{children}</ol>,
  code: ({ children }) => <code className="rounded bg-muted px-1 font-mono text-[0.9em]">{children}</code>,
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer" className="underline underline-offset-2">
      {children}
    </a>
  ),
};

// A newer Haul, in the status bar: its release notes, and in the desktop app
// one button that downloads it, checks it and installs it over this one.
export function UpdateEntry({ s }: { s: State }) {
  const [open, setOpen] = useState(false);
  const [installing, setInstalling] = useState(false);
  const u = s.update;
  if (!u) return null;
  const install = async () => {
    setInstalling(true);
    try {
      // The app closes once the setup program starts; it opens again after.
      await update.install();
    } catch (error) {
      report(error);
      setInstalling(false);
    }
  };
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="inline-flex h-7 items-center gap-1.5 rounded-md bg-info-soft px-2 text-xs font-medium text-info-strong hover:brightness-95"
      >
        <ArrowUpCircle aria-hidden className="size-3.5" />
        Update to {u.version}
      </button>
      <Dialog open={open} onOpenChange={(next) => !installing && setOpen(next)}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>Haul {u.version}</DialogTitle>
            <DialogDescription>
              You have {s.version}.{' '}
              {s.desktop && u.installable
                ? 'Installing downloads it, checks it against its checksums, closes Haul and opens the new version; downloads pick up where they stopped.'
                : 'Download it from its release page.'}
            </DialogDescription>
          </DialogHeader>
          {u.notes ? (
            <div className="max-h-80 overflow-y-auto rounded-md border bg-muted/40 p-3 text-sm">
              <Markdown components={NOTES}>{u.notes}</Markdown>
            </div>
          ) : null}
          <DialogFooter>
            <Button variant="outline" asChild>
              <a href={u.page} target="_blank" rel="noreferrer">
                <ExternalLink aria-hidden />
                Release page
              </a>
            </Button>
            {s.desktop && u.installable ? (
              <Button onClick={install} disabled={installing}>
                {installing ? <LoaderCircle aria-hidden className="animate-spin" /> : <ArrowUpCircle aria-hidden />}
                {installing ? 'Installing…' : 'Install and restart'}
              </Button>
            ) : null}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
