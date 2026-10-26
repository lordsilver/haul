import { Upload } from 'lucide-react';
import { type FormEvent, useEffect, useRef } from 'react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { addText, closeAdd, editAdd, readFiles, useAddForm } from '@/lib/add';

export function AddDialog() {
  const form = useAddForm();
  const text = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  // Back in the links whenever more come in.
  useEffect(() => {
    if (form.open && form.focus) setTimeout(() => text.current?.focus(), 0);
  }, [form.open, form.focus]);
  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (await addText(form.text, form.pkg.trim())) closeAdd();
  };
  return (
    <Dialog open={form.open} onOpenChange={(open) => !open && closeAdd()}>
      <DialogContent className="sm:max-w-xl">
        <form className="flex flex-col gap-4" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>Add links</DialogTitle>
            <DialogDescription>
              Paste anywhere in Haul, or drop links and .dlc / .txt files on the window.
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-2">
            <Label htmlFor="add-text">Links, or any text with links in it</Label>
            <Textarea
              id="add-text"
              ref={text}
              rows={7}
              required
              spellCheck={false}
              className="max-h-80 font-mono text-xs"
              placeholder={'https://pixeldrain.com/u/…\nhttps://gofile.io/d/…'}
              value={form.text}
              onChange={(e) => editAdd({ text: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                  e.preventDefault();
                  submit();
                }
              }}
            />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="add-package">
              Package <span className="font-normal text-muted-foreground">optional subfolder</span>
            </Label>
            <Input
              id="add-package"
              autoComplete="off"
              value={form.pkg}
              onChange={(e) => editAdd({ pkg: e.target.value })}
            />
          </div>
          <DialogFooter className="sm:justify-between">
            <Button type="button" variant="outline" onClick={() => file.current?.click()}>
              <Upload />
              Import file…
            </Button>
            <input
              ref={file}
              type="file"
              accept=".dlc,.txt,.crawljob"
              multiple
              hidden
              onChange={async (e) => {
                if (e.target.files) await readFiles(e.target.files);
                e.target.value = '';
              }}
            />
            <div className="flex gap-2">
              <Button type="button" variant="ghost" onClick={closeAdd}>
                Cancel
              </Button>
              <Button type="submit">Add</Button>
            </div>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
