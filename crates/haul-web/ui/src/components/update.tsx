import { ArrowUpCircle, ExternalLink } from 'lucide-react';
import { useState } from 'react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { api, report } from '@/lib/api';
import type { State } from '@/lib/bindings/State';

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
      await api('/api/update/install');
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
            <div className="max-h-80 overflow-y-auto rounded-md border bg-muted/40 p-3 text-sm whitespace-pre-wrap">
              {u.notes.trim()}
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
                <ArrowUpCircle aria-hidden />
                {installing ? 'Downloading…' : 'Install and restart'}
              </Button>
            ) : null}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
