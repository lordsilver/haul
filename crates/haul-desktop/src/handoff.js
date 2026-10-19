// Hands Haul a host's files as soon as its page has them, so the person at
// most passes the page's check and never looks for a download button. The
// page navigates to an address the window never loads; the app reads the
// files (link, name, size) and the window's user agent from it, or that the
// page says its file is gone. In a browser Haul drives over DevTools (Edge
// on Windows, chromium.rs), which sets __haulDevTools first, the message
// waits in __haulHandoff instead, for it to read.
//
// One recipe per host whose page passes this way (hosts::Known::page_passes
// in haul-core says which): where on the page the links turn up.
(() => {
  // The page itself, never its frames (see overlays.js).
  if (window !== window.top) return;
  let handed = false;
  const send = (message, address) => {
    window.__haulHandoff = message;
    if (!window.__haulDevTools) location.href = address;
  };
  const hand = (files) => {
    if (handed || !files.length) return;
    handed = true;
    const absolute = files.map((f) => ({ ...f, url: new URL(f.url, location.href).href }));
    send({ files: absolute, agent: navigator.userAgent }, "https://haul.invalid/take?" + new URLSearchParams({ files: JSON.stringify(absolute), agent: navigator.userAgent }));
  };

  // The page passed its check but could not give the link this time (a
  // host's rate limit, an error it shows): Haul tries the page again later.
  const later = (reason) => {
    if (handed) return;
    handed = true;
    send({ later: reason }, "https://haul.invalid/later?" + new URLSearchParams({ reason }));
  };

  // The page says the file is gone: the mirror failed, nobody is asked.
  const gone = (reason) => {
    if (handed) return;
    handed = true;
    send({ gone: reason }, "https://haul.invalid/gone?" + new URLSearchParams({ reason }));
  };

  const recipes = [
    {
      // Once its Turnstile check passes, the page fetches the file's link
      // and sets it on its download button.
      site: /(^|\.)(vikingfile\.com|vik1ngfile\.site)$/,
      // After the check, the button reads "Generating download link…"
      // until the link comes, or "Error : …" when the page's request for it
      // fails; a request that never answers leaves it generating.
      watch() {
        let since = 0;
        const look = () => {
          const button = document.getElementById("download-link");
          if (!button) return;
          const url = button.getAttribute("href");
          if (url && url !== "#") {
            const name = document.getElementById("filename")?.textContent.trim() || undefined;
            return hand([{ url, name }]);
          }
          const text = button.textContent.trim();
          if (/^error\b/i.test(text)) return later(`vikingfile: ${text.replace(/^error\s*:\s*/i, "") || "no link this time"}`);
          if (!button.classList.contains("hidden") && !since) since = Date.now();
          if (since && Date.now() - since > 45000) later("vikingfile: its page did not hand out the link");
        };
        new MutationObserver(look).observe(document, { subtree: true, attributes: true, childList: true, characterData: true });
        setInterval(look, 5000);
      },
    },
    {
      // The page signs its own request for the folder (a token from a
      // secret gofile rotates) and lists the files it answers with; the
      // answer is read as it arrives. Subfolders are left to their own links.
      site: /(^|\.)gofile\.io$/,
      watch() {
        const fetched = window.fetch;
        window.fetch = async function (...args) {
          const response = await fetched.apply(this, args);
          const url = String(args[0]?.url ?? args[0] ?? "");
          if (/^https:\/\/api\.gofile\.io\/contents\/[^/?]+\?/.test(url)) {
            response
              .clone()
              .json()
              .then((answer) => {
                if (answer?.status === "error-notFound") return gone("gofile: the folder is gone");
                if (answer?.status !== "ok") return;
                const data = answer.data ?? {};
                const nodes = data.type === "file" ? [data] : Object.values(data.children ?? {});
                hand(nodes.filter((n) => n.type === "file" && n.link).map((n) => ({ url: n.link, name: n.name, size: n.size })));
              })
              .catch(() => {});
          }
          return response;
        };
      },
    },
  ];

  recipes.find((r) => r.site.test(location.hostname))?.watch();
})();
