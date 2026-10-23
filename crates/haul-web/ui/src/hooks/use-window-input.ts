import { useEffect, useState } from 'react';
import { addOpen, addText, importFiles, openAdd, readFiles } from '@/lib/add';
import { api, report } from '@/lib/api';
import { currentState } from '@/lib/store';
import { toast } from '@/lib/toast';

const carriesData = (e: DragEvent) =>
  [...(e.dataTransfer?.types ?? [])].some((t) => t === 'Files' || t.startsWith('text/'));

// Links pasted anywhere outside a text field, and links or container files
// dropped anywhere on the window. Answers whether something is being dragged
// over the window, for the drop overlay.
export function useWindowInput(): boolean {
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    // With one download waiting for a person, a single pasted link is the
    // one its page just handed back: it goes to that download rather than
    // starting a new one.
    const paste = (e: ClipboardEvent) => {
      if ((e.target as HTMLElement).closest?.('input, textarea') || addOpen()) return;
      const text = e.clipboardData?.getData('text') ?? '';
      if (!/https?:\/\//i.test(text)) return;
      e.preventDefault();
      const link = text.trim();
      const waiting = currentState()?.items.filter((i) => i.status === 'captcha') ?? [];
      const it = waiting[0];
      if (waiting.length === 1 && it && !/\s/.test(link)) {
        api(`/api/item/${encodeURIComponent(it.id)}/direct`, { url: link }).then(
          () => toast(`Using your link for ${it.name}`, 'success'),
          report,
        );
        return;
      }
      openAdd(text);
    };

    // The whole window lights up as the drop zone while something is dragged
    // over. A host being reordered carries a type of its own and drops on
    // its list.
    let depth = 0;
    const enter = (e: DragEvent) => {
      if (!carriesData(e) || addOpen()) return;
      depth += 1;
      setDragging(true);
    };
    const end = () => {
      depth = 0;
      setDragging(false);
    };
    const leave = () => {
      depth = Math.max(0, depth - 1);
      if (!depth) end();
    };
    const over = (e: DragEvent) => {
      if (carriesData(e)) e.preventDefault();
    };
    // Dropped links and files fill the Add dialog, like pasted ones, unless
    // the settings say to add them at once.
    const drop = (e: DragEvent) => {
      end();
      if (!carriesData(e) || !e.dataTransfer) return;
      const data = e.dataTransfer;
      const now = Boolean(currentState()?.skip_add_dialog);
      if (data.files.length) {
        e.preventDefault();
        (now ? importFiles : readFiles)(data.files);
      } else if (!(e.target as HTMLElement).closest?.('textarea, input')) {
        e.preventDefault();
        const text = data.getData('text/uri-list') || data.getData('text/plain');
        if (now) addText(text);
        else openAdd(text);
      }
    };

    document.addEventListener('paste', paste);
    addEventListener('dragenter', enter);
    addEventListener('dragleave', leave);
    addEventListener('dragover', over);
    addEventListener('drop', drop);
    return () => {
      document.removeEventListener('paste', paste);
      removeEventListener('dragenter', enter);
      removeEventListener('dragleave', leave);
      removeEventListener('dragover', over);
      removeEventListener('drop', drop);
    };
  }, []);

  return dragging;
}
