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
  // An address as Haul names it: in stand-in mode (browser.rs sets
  // __haulStandIn), `http://<site>.localhost:<port>/…` stands for
  // `https://<site>/…`, as hosts::reach in haul-core has it.
  const real = (address) =>
    window.__haulStandIn ? address.replace(/^http:\/\/([^/?#]+)\.localhost(:\d+)?(?=[/?#]|$)/i, "https://$1") : address;
  // The page's site, as the recipes name it.
  const site = new URL(real(location.href)).hostname;
  let handed = false;
  const send = (message, address) => {
    window.__haulHandoff = message;
    if (!window.__haulDevTools) location.href = address;
  };
  // `page`: on a wrapper's page Haul's own code reads, the parts of it that
  // code reads, which stand in for the files when Haul finds what it holds.
  const hand = (files, page) => {
    if (handed || !files.length) return;
    handed = true;
    const absolute = files.map((f) => ({ ...f, url: new URL(f.url, location.href).href }));
    const message = { files: absolute, agent: navigator.userAgent, ...(page ? { page } : {}) };
    send(message, "https://haul.invalid/take?" + new URLSearchParams({ ...message, files: JSON.stringify(absolute) }));
  };

  // The page passed its check but could not give the link this time (a
  // host's rate limit, an error it shows): Haul tries the page again later.
  // `seconds`: the page says how long its host limits this address, which
  // holds for all of that host's pages.
  const later = (reason, seconds) => {
    if (handed) return;
    handed = true;
    const wait = seconds ? { seconds } : {};
    send({ later: reason, ...wait }, "https://haul.invalid/later?" + new URLSearchParams({ reason, ...wait }));
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
          if (/^https:\/\/api\.gofile\.io\/contents\/[^/?]+\?/.test(real(url))) {
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
      // A 60-second countdown and Cloudflare's Turnstile in the download
      // form; once the button is enabled and the Turnstile has its answer,
      // the button sends the form. The site answers with the file, which the
      // browser reports to Haul (`Cdp::take_downloads`); a page that links it
      // instead is read below.
      site: /(^|\.)(ddownload\.com|ddl\.to)$/,
      watch() {
        let pressed = false;
        const name = () => document.querySelector(".dk-dl-name")?.getAttribute("title") || undefined;
        const look = () => {
          const text = document.body?.innerText ?? "";
          if (/\bFile Not Found\b/.test(text)) return gone("ddownload: the file was deleted");
          const button = document.querySelector("#downloadbtn");
          // One free download per address, about every two hours: until then
          // the button only opens a payment dialog, and the page says how
          // long is left (two hours if it does not).
          if (button?.classList.contains("dk-btn-blocked")) {
            const left = Math.ceil(Number(button.getAttribute("data-wait-seconds")));
            return later("ddownload: one free download per address, and this one's limit is in force", left > 0 ? left : 7200);
          }
          const answer = document.querySelector('input[name="cf-turnstile-response"]');
          if (button && !pressed) {
            if (!button.disabled && !button.classList.contains("dk-btn-disabled") && answer?.value) {
              pressed = true;
              button.click();
            }
            return;
          }
          // The file's link: an address on the host's servers that ends in
          // a file name, not another page.
          const link = [...document.querySelectorAll("a[href]")]
            .map((a) => a.href)
            .find((h) => /^https?:\/\/[^/]*ddownload\.com\/.+\/[^/?#]+\.[A-Za-z0-9]{2,5}(\?|$)/.test(h) && !/\.(html?|php)(\?|$)/i.test(h));
          if (link) hand([{ url: link, name: name() }]);
        };
        setInterval(look, 1000);
      },
    },
    {
      // Past Cloudflare's check (`?action=check`), the file's page has the
      // download address on its button: base64 in `data-scrambled-url` on
      // newer pages, else its `href` (as hosts/mediafire.rs reads it). A
      // file mediafire took down sends the page to `error.php`.
      site: /(^|\.)mediafire\.com$/,
      watch() {
        if (/\/error\.php$/.test(location.pathname)) return gone("mediafire: the file is not available");
        const look = () => {
          const button = document.getElementById("downloadButton");
          if (!button) return;
          let url = null;
          try {
            const scrambled = button.getAttribute("data-scrambled-url");
            url = scrambled ? atob(scrambled) : button.getAttribute("href");
          } catch {}
          if (url && /^https?:\/\//.test(url)) hand([{ url }]);
        };
        new MutationObserver(look).observe(document, { subtree: true, childList: true, attributes: true });
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
    {
      // Past its gate (a picture puzzle a person solves here, or none when
      // the browser is let through), the container's page shows its links:
      // as Click'n'Load data, which Haul decrypts and names after the rows
      // of the page's table (hosts/filecrypt.rs reads the heading, the rows
      // and that data, handed here as they show), and as a `.dlc`, whose
      // button names the attribute holding its id
      // (`DownloadDLC(this.getAttribute('data-…'))`). That link is not
      // gated, and stands in when Haul finds nothing in the page.
      site: /(^|\.)filecrypt\.cc$/,
      watch() {
        // Out of sight, "Not Found" ends the pass instead of waiting out its
        // time for nobody. The site says it of a deleted container and to an
        // address it refuses alike, so the row fails for now, not for good;
        // a person's window just shows the page. Cloudflare's check in front
        // of the page answers 403, and this runs again on the page after it.
        const notFound = () => {
          if (handed || !window.__haulDevTools) return;
          const answered = performance.getEntriesByType("navigation")[0]?.responseStatus;
          if (answered !== 404 && location.pathname.toLowerCase() !== "/404.html") return;
          handed = true;
          window.__haulHandoff = {
            failed: "filecrypt.cc: answered Not Found: the container was deleted, or the site is refusing this browser for now",
          };
        };
        // The status may not be there yet as the document starts.
        notFound();
        document.addEventListener("DOMContentLoaded", notFound);
        const look = () => {
          // The link table comes after the buttons: read the page once it
          // is all there, not as it is parsed.
          if (handed || document.readyState === "loading") return;
          const button = document.querySelector('[onclick*="DownloadDLC"]');
          const attribute = button?.getAttribute("onclick").match(/getAttribute\(\s*['"]([\w-]+)['"]\s*\)/)?.[1];
          const id = attribute && button.getAttribute(attribute);
          if (!id || !/^[\w-]+$/.test(id)) return;
          const read = [document.querySelector("h2"), ...document.querySelectorAll('tr, [onclick*="CNLPOP"], [onsubmit*="CNLPOP"]')].filter(Boolean);
          // Each once: a part inside another (the Click'n'Load button in a
          // row) comes with it.
          const page = read
            .filter((e) => !read.some((o) => o !== e && o.contains(e)))
            .map((e) => e.outerHTML)
            .join("\n");
          hand([{ url: `/DLC/${id}.dlc` }], page);
        };
        new MutationObserver(look).observe(document, { subtree: true, childList: true });
        document.addEventListener("DOMContentLoaded", look);
        look();
      },
    },
  ];

  // A page on a site Haul has no code for (a post that lists links, a paste), which
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
    // What was last offered: the links themselves, so a page with as many
    // as the one before (a paste's list after its gate) is offered too.
    let offered = "";
    let timer;
    const look = () => {
      // Addresses written out in the page's text apart: on a paste they are
      // its list, whatever host they are on; a button is as likely an ad.
      const text = new Set();
      for (const m of (document.body?.innerText ?? "").matchAll(/https?:\/\/[^\s"'<>()[\]{}]+/g)) text.add(m[0]);
      const links = new Set(text);
      for (const a of document.querySelectorAll("a[href]")) if (/^https?:/.test(a.href)) links.add(a.href);
      const key = [...links].join("\n");
      if (key === offered) return;
      offered = key;
      const all = [...links];
      const shown = [...text];
      const title = document.title.trim();
      send(
        { links: all, shown, title },
        "https://haul.invalid/links?" +
          new URLSearchParams({ links: JSON.stringify(all), shown: JSON.stringify(shown), title }),
      );
    };
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(look, 700);
    };
    // A link's address set by a script (after a gate, say) counts as much as
    // a new one.
    new MutationObserver(soon).observe(document, {
      subtree: true,
      childList: true,
      characterData: true,
      attributes: true,
      attributeFilter: ["href"],
    });
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

  // A site with no recipe of its own, passed out of sight for its file: the
  // general way. What a person would press (the page's download, after any
  // countdown) is pressed, at most four times over the pages it leads to; a
  // link that names a file is handed over as it is. Nothing here knows a
  // site: words, links and what can be pressed decide.
  if (window.__haulLists && window.__haulDevTools && !recipes.some((r) => r.site.test(site))) {
    const SAYS_DOWNLOAD =
      /\b(download|t[ée]l[ée]charger|herunterladen|descargar|scaricare|scarica|baixar|скачать|get (the )?file|free download|slow download|start download|generate (the )?link|continue to (the )?download)\b|下载|ダウンロード/i;
    const NOT_FREE = /\b(premium|fast(er)? download|high speed|buy|upgrade|vip|torrent|install|extension|register|sign ?up|log ?in|subscribe)\b/i;
    const FILE = /\.(rar|zip|7z|001|00\d|r\d\d|tar|gz|bz2|xz|zst|iso|img|bin|exe|msi|dmg|apk|mkv|mp4|avi|mov|webm|mp3|flac|pdf|epub)(\?|$)/i;
    const presses = () => Number(sessionStorage.getItem("__haulPresses") || 0);
    const pressed = () => sessionStorage.setItem("__haulPresses", String(presses() + 1));
    const names = (href) => {
      try {
        return FILE.test(new URL(href, location.href).pathname);
      } catch {
        return false;
      }
    };
    const shown = (e) => {
      const box = e.getBoundingClientRect();
      const style = getComputedStyle(e);
      return box.width > 4 && box.height > 4 && style.visibility !== "hidden" && style.display !== "none";
    };
    const waiting = (e) =>
      e.disabled || e.getAttribute("aria-disabled") === "true" || /\b(disabled|wait|countdown)\b/i.test(e.className);
    // How much an element looks like the page's own way to the file.
    const score = (e) => {
      const words = (e.innerText || e.value || e.getAttribute("aria-label") || e.title || "").trim();
      if (!words || words.length > 60 || NOT_FREE.test(words)) return 0;
      let points = SAYS_DOWNLOAD.test(words) ? 10 : 0;
      if (e.tagName === "A") {
        if (names(e.href)) points += 8;
        else if (new URL(e.href, location.href).hostname !== location.hostname) points -= 6;
        if (e.target && e.target !== "_self") points -= 3;
      }
      if (e.closest("form")) points += 2;
      return points;
    };
    let rest = 0;
    window.__haulPressing = false;
    setInterval(() => {
      if (handed || Date.now() < rest) return;
      // A gate only a person passes: nothing to press until they have.
      if (document.querySelector('iframe[src*="recaptcha"], iframe[src*="hcaptcha"], .g-recaptcha, .h-captcha')) {
        window.__haulPressing = false;
        return;
      }
      // A link that names a file is the file: Haul fetches it itself. Several
      // are a list (an archive's parts), which the page's links hand over.
      const files = [
        ...new Set(
          [...document.querySelectorAll("a[href]")]
            .filter((a) => shown(a) && names(a.href) && !NOT_FREE.test(a.innerText || ""))
            .map((a) => a.href),
        ),
      ];
      if (files.length > 1) {
        window.__haulPressing = false;
        return;
      }
      if (files.length === 1) return hand([{ url: files[0], name: decodeURIComponent(new URL(files[0]).pathname.split("/").pop()) || undefined }]);
      const best = [...document.querySelectorAll('a[href], button, input[type="submit"], input[type="button"], [role="button"]')]
        .filter(shown)
        .map((e) => ({ e, points: score(e) }))
        .filter((c) => c.points >= 10)
        .sort((a, b) => b.points - a.points)[0];
      window.__haulPressing = Boolean(best) && presses() < 4;
      if (!best || presses() >= 4 || waiting(best.e)) return;
      pressed();
      rest = Date.now() + 5000;
      if (best.e.tagName === "A" && best.e.target && best.e.target !== "_self") best.e.target = "_self";
      best.e.click();
    }, 1500);
  }

  // Out of sight, a page past every check that shows nothing new for five
  // seconds has shown all it will (a paste holding only a host's name), so
  // the pass can end rather than wait out its time for a person.
  if (window.__haulLists && window.__haulDevTools) {
    let calm = 0;
    let seen = "";
    setInterval(() => {
      const check =
        window._cf_chl_opt ||
        document.getElementById("challenge-form") ||
        document.querySelector('input[name="cf-turnstile-response"], iframe[src*="challenges.cloudflare.com"]');
      const links = (window.__haulHandoff?.links ?? []).join("\n");
      // Not while the general way still has something to press or wait for.
      calm = document.readyState === "complete" && !check && !window.__haulPressing && links === seen ? calm + 1 : 0;
      seen = links;
      if (calm === 5) window.__haulHandoff = { links: [], shown: [], settled: true };
    }, 1000);
  }

  recipes.find((r) => r.site.test(site))?.watch();
})();
