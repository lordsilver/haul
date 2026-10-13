// Applies the saved theme before the first paint, so the page never
// flashes the other one. app.js keeps it in sync afterwards.
try {
  const theme = localStorage.getItem("haul.theme");
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
} catch {}
