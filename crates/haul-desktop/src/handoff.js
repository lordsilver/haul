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
      // Behind Cloudflare's check, the page's "Download File" button asks
      // for the file with htmx (`hx-get="/<id>/download?t=…"`) and is
      // answered with where the file is (`HX-Redirect`, as buzzheavier's).
      // The request goes from the page, with its cookies. The heading, or
      // the link's part after #, names the file.
      site: /(^|\.)fuckingfast\.(net|co)$/,
      watch() {
        let asked = false;
        const look = async () => {
          const button = document.querySelector("a.download-btn[hx-get], a[hx-get*='/download?']");
          if (asked || !button) return;
          asked = true;
          const ask = new URL(button.getAttribute("hx-get"), location.href).href;
          const fromLink = decodeURIComponent(location.hash.slice(1)) || undefined;
          const heading = document.querySelector("h1, h2")?.textContent.trim() || undefined;
          const name = fromLink || heading;
          try {
            const answer = await fetch(ask, { headers: { "HX-Request": "true", "HX-Current-URL": location.href } });
            const to = answer.headers.get("HX-Redirect") || (answer.redirected ? answer.url : null);
            if (to) return hand([{ url: new URL(to, location.href).href, name }]);
            const text = await answer.text();
            const found = text.match(/https?:\/\/[^"'\s<>]+/)?.[0];
            if (found) return hand([{ url: found, name }]);
          } catch (_) {}
          // Without an answer to read, the address the button asks is the
          // download itself.
          hand([{ url: ask, name }]);
        };
        new MutationObserver(look).observe(document, { subtree: true, childList: true });
        look();
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
    let offered = 0;
    let timer;
    const look = () => {
      // Addresses written out in the page's text apart: on a paste they are
      // its list, whatever host they are on; a button is as likely an ad.
      const text = new Set();
      for (const m of (document.body?.innerText ?? "").matchAll(/https?:\/\/[^\s"'<>()[\]{}]+/g)) text.add(m[0]);
      const links = new Set(text);
      for (const a of document.querySelectorAll("a[href]")) if (/^https?:/.test(a.href)) links.add(a.href);
      if (links.size <= offered) return;
      offered = links.size;
      const all = [...links];
      const shown = [...text];
      send(
        { links: all, shown },
        "https://haul.invalid/links?" + new URLSearchParams({ links: JSON.stringify(all), shown: JSON.stringify(shown) }),
      );
    };
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(look, 700);
    };
    new MutationObserver(soon).observe(document, { subtree: true, childList: true, characterData: true });
    soon();

    // Pastes often hold their links behind a Turnstile of their own and a
    // button to press once it passed (zpaste.net's "Verify & continue"):
    // once the check has its answer, its form is sent, once.
    let sent = false;
    setInterval(() => {
      // Cloudflare's own check page sends itself: hands off.
      if (sent || window._cf_chl_opt || document.getElementById("challenge-form")) return;
      const answer = document.querySelector('input[name="cf-turnstile-response"]');
      if (!answer?.value) return;
      const form = answer.closest("form");
      const button = form?.querySelector('button[type="submit"], input[type="submit"], button:not([type])');
      if (!form) return;
      sent = true;
      if (button) button.click();
      else form.requestSubmit();
    }, 1000);
  }

  recipes.find((r) => r.site.test(location.hostname))?.watch();
})();
