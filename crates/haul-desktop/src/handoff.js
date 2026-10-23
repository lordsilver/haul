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
                if (answer?.status === "error-notFound") return gone("gofile: the folder was deleted");
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
    {
      // A person solves 1fichier's captcha here and the download starts by
      // itself; all the page can say first is that the file was deleted
      // (its own words, French then English, with the 404 it answers).
      site: /(^|\.)1fichier\.com$/,
      watch() {
        const look = () => {
          const text = document.body?.innerText ?? "";
          if (/\b(File not found|Fichier introuvable)\b/.test(text)) gone("1fichier: the file was deleted");
        };
        if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", look);
        else look();
      },
    },
  ];

  // A page on a site Haul has no code for (a release's post, a paste), which
  // kept Haul out with a check a person passes here: every address it shows
  // goes to Haul, which keeps those on hosts it knows and wrappers. Until it
  // finds some the window stays, so the person can go on to the page that
  // has them, in this window: a link meant for a new one opens here, as the
  // window takes no new ones (ads open those).
  if (window.__haulLists) {
    document.addEventListener(
      "click",
      (e) => {
        const a = e.target instanceof Element ? e.target.closest("a[target]") : null;
        if (a && a.target !== "_self") a.target = "_self";
      },
      true,
    );
    let shown = 0;
    let timer;
    const look = () => {
      const links = new Set();
      for (const a of document.querySelectorAll("a[href]")) if (/^https?:/.test(a.href)) links.add(a.href);
      for (const m of (document.body?.innerText ?? "").matchAll(/https?:\/\/[^\s"'<>()[\]{}]+/g)) links.add(m[0]);
      if (links.size <= shown) return;
      shown = links.size;
      const all = [...links];
      send({ links: all }, "https://haul.invalid/links?" + new URLSearchParams({ links: JSON.stringify(all) }));
    };
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(look, 700);
    };
    new MutationObserver(soon).observe(document, { subtree: true, childList: true, characterData: true });
    soon();
  }

  recipes.find((r) => r.site.test(location.hostname))?.watch();
})();
