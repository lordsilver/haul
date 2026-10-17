// Hands Haul a host's files as soon as its page has them, so the person at
// most passes the page's check and never looks for a download button. The
// page navigates to an address the window never loads; the app reads the
// files (link, name, size) and the window's user agent from it, or that the
// page says its file is gone.
//
// One recipe per host whose page passes this way (hosts::Known::page_passes
// in haul-core says which): where on the page the links turn up.
(() => {
  let handed = false;
  const hand = (files) => {
    if (handed || !files.length) return;
    handed = true;
    const absolute = files.map((f) => ({ ...f, url: new URL(f.url, location.href).href }));
    location.href = "https://haul.invalid/take?" + new URLSearchParams({ files: JSON.stringify(absolute), agent: navigator.userAgent });
  };

  // The page says the file is gone: the mirror failed, nobody is asked.
  const gone = (reason) => {
    if (handed) return;
    handed = true;
    location.href = "https://haul.invalid/gone?" + new URLSearchParams({ reason });
  };

  const recipes = [
    {
      // Once its Turnstile check passes, the page fetches the file's link
      // and sets it on its download button.
      site: /(^|\.)(vikingfile\.com|vik1ngfile\.site)$/,
      watch() {
        const look = () => {
          const url = document.getElementById("download-link")?.getAttribute("href");
          if (!url) return;
          const name = document.getElementById("filename")?.textContent.trim() || undefined;
          hand([{ url, name }]);
        };
        new MutationObserver(look).observe(document, { subtree: true, attributes: true, attributeFilter: ["href"] });
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
