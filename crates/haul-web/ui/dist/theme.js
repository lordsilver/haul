// Applies the saved theme before the first paint, so the page never flashes
// the other one. The app keeps it in step afterwards (lib/theme.ts).
try {
  const saved = localStorage.getItem('haul.theme');
  const dark = saved === 'dark' || (saved !== 'light' && matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.classList.toggle('dark', dark);
} catch {}
