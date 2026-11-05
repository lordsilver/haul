import { useSyncExternalStore } from 'react';

// State kept outside React: `use` is the hook that renders what `read` answers,
// `notify` tells every mounted one that it changed.
export function externalStore<T>(read: () => T) {
  const listeners = new Set<() => void>();
  const subscribe = (listener: () => void) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  };
  return {
    use: () => useSyncExternalStore(subscribe, read),
    notify: () => {
      for (const listener of listeners) listener();
    },
  };
}
