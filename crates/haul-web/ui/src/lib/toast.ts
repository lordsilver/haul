import { toast as sonner } from 'sonner';

export type ToastKind = 'info' | 'success' | 'error';

export function toast(message: string, kind: ToastKind = 'info') {
  const options = { duration: kind === 'error' ? 7000 : 4000 };
  if (kind === 'success') sonner.success(message, options);
  else if (kind === 'error') sonner.error(message, options);
  else sonner.info(message, options);
}
