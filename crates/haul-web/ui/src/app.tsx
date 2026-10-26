import { Download } from 'lucide-react';
import { useEffect } from 'react';
import { AddDialog } from '@/components/add-dialog';
import { Sidebar } from '@/components/sidebar';
import { StatusBar } from '@/components/status-bar';
import { Toaster } from '@/components/ui/sonner';
import { TooltipProvider } from '@/components/ui/tooltip';
import { useWindowInput } from '@/hooks/use-window-input';
import { bytes } from '@/lib/format';
import { isList, useView } from '@/lib/route';
import { useLive } from '@/lib/store';
import { applyTheme } from '@/lib/theme';
import { Connections } from '@/views/connections';
import { Downloads } from '@/views/downloads';
import { Hosts } from '@/views/hosts';
import { Settings } from '@/views/settings';
import { Wrappers } from '@/views/wrappers';

// The whole window takes drops while links or files are dragged over it.
function DropOverlay() {
  return (
    <div className="pointer-events-none fixed inset-0 z-50 grid place-items-center bg-background/70 p-6 backdrop-blur-sm">
      <div className="flex max-w-sm flex-col items-center gap-2 rounded-xl border-2 border-dashed bg-background p-8 text-center shadow-lg">
        <Download aria-hidden className="size-8 text-info" />
        <strong>Drop to add to Haul</strong>
        <span className="text-sm text-muted-foreground">Links, .dlc containers and text files with links in them.</span>
      </div>
    </div>
  );
}

export function App() {
  const { state: s, offline, updated } = useLive();
  const view = useView();
  const dragging = useWindowInput();

  const theme = s?.theme;
  const speed = s?.speed ?? 0;
  useEffect(() => {
    if (theme) applyTheme(theme);
  }, [theme]);
  useEffect(() => {
    document.title = speed ? `${bytes(speed)}/s · Haul` : 'Haul';
  }, [speed]);

  return (
    <TooltipProvider delayDuration={400}>
      <div className="flex h-dvh flex-col">
        <div className="flex min-h-0 flex-1">
          <Sidebar state={s} view={view} />
          <main className="flex min-w-0 flex-1 flex-col gap-4 overflow-hidden px-8 pt-6 pb-5 max-md:px-4">
            {!s ? null : isList(view) ? (
              <Downloads s={s} view={view} offline={offline} updated={updated} />
            ) : view === 'wrappers' ? (
              <Wrappers s={s} />
            ) : view === 'hosts' ? (
              <Hosts s={s} />
            ) : view === 'connections' ? (
              <Connections s={s} />
            ) : (
              <Settings s={s} />
            )}
          </main>
        </div>
        <StatusBar state={s} />
      </div>
      <AddDialog />
      {dragging ? <DropOverlay /> : null}
      <Toaster position="bottom-right" visibleToasts={4} closeButton />
    </TooltipProvider>
  );
}
