import { toast as sonner } from 'sonner';

export type ToastKind = 'info' | 'success' | 'error';

// What a toast offers: a button (`onClick`) or a page to open (`href`).
export type ToastAction = { label: string; onClick?: () => unknown; href?: string };

// A toast can offer one action; one that does stays longer, to be read and
// used. A toast with a `key` takes the place of the one with the same key
// still showing, so news that keeps coming updates one toast instead of
// stacking more. `onGone` runs once it is gone, closed or timed out.
export function toast(
  message: string,
  kind: ToastKind = 'info',
  action?: ToastAction,
  key?: string,
  onGone?: () => void,
) {
  const duration = action ? 12000 : kind === 'error' ? 7000 : 4000;
  const options = {
    duration,
    ...(key ? { id: key } : {}),
    ...(onGone ? { onDismiss: onGone, onAutoClose: onGone } : {}),
    ...(action
      ? {
          action: {
            label: action.label,
            onClick: () => {
              if (action.href) window.open(action.href, '_blank', 'noopener,noreferrer');
              action.onClick?.();
            },
          },
        }
      : {}),
  };
  if (kind === 'success') sonner.success(message, options);
  else if (kind === 'error') sonner.error(message, options);
  else sonner.info(message, options);
}
