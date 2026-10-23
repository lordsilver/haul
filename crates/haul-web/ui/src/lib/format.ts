const UNITS: [number, string, number][] = [
  [1e12, 'TB', 2],
  [1e9, 'GB', 2],
  [1e6, 'MB', 1],
  [1e3, 'KB', 0],
];

export function bytes(n: number): string {
  for (const [size, unit, digits] of UNITS) if (n >= size) return `${(n / size).toFixed(digits)} ${unit}`;
  return `${Math.round(n)} B`;
}

export function duration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

export function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

export function ordinal(n: number): string {
  return n === 1 ? '1st' : n === 2 ? '2nd' : n === 3 ? '3rd' : `${n}th`;
}

export const isWebLink = (url: string) => /^https?:\/\//i.test(url);

// A link's host, without "www.", for a button that names it.
export function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, '');
  } catch {
    return 'page';
  }
}

// A time to wait in round words: "about 5 min".
export function roughly(seconds: number): string {
  const min = Math.round(seconds / 60);
  if (min < 1) return 'under a minute';
  if (min < 60) return `about ${min} min`;
  const h = Math.floor(min / 60);
  return `about ${h} h${min % 60 ? ` ${min % 60} min` : ''}`;
}
