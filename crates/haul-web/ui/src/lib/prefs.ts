// localStorage can be missing or throw (private windows, blocked site data).
export const prefs = {
  get(key: string): string | null {
    try {
      return localStorage.getItem(`haul.${key}`);
    } catch {
      return null;
    }
  },
  set(key: string, value: string) {
    try {
      localStorage.setItem(`haul.${key}`, value);
    } catch {}
  },
};
