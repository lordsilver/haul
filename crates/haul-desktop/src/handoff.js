// Hands Haul a file's link as soon as a host's page has it, so the person
// only passes the page's check and never has to find the download button.
// The page navigates to an address the window never loads; the app reads
// the link (and the file's name) from it and takes the download.
//
// vikingfile: once its Turnstile check passes, the page fetches the link
// and sets it on #download-link.
(() => {
  if (!/(^|\.)(vikingfile\.com|vik1ngfile\.site)$/.test(location.hostname)) return;
  let handed = false;
  const hand = () => {
    const link = document.getElementById("download-link")?.getAttribute("href");
    if (handed || !link) return;
    handed = true;
    const name = document.getElementById("filename")?.textContent.trim() ?? "";
    location.href = "https://haul.invalid/take?" + new URLSearchParams({ url: new URL(link, location.href).href, name });
  };
  new MutationObserver(hand).observe(document, { subtree: true, attributes: true, attributeFilter: ["href"] });
})();
