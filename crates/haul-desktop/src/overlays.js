// Run in the in-app browser's pages before their own scripts. Ads lay a
// layer over the page, an ad's frame or an invisible sheet, so the first
// click lands on the ad instead of the button under it. A layer above
// everything that covers much of the window is taken off, unless it holds a
// captcha, which also floats above the page.
(() => {
  // The page itself, never its frames: WebView2 runs this in every frame,
  // and a script at work inside a captcha's frame (Cloudflare Turnstile's)
  // makes it ask a person.
  if (window !== window.top) return;
  if (!/^https?:$/.test(location.protocol)) return;
  const CAPTCHA = /captcha|challenges\.cloudflare\.com|turnstile/i;
  const isAd = (el) => {
    const style = getComputedStyle(el);
    if (style.position !== "fixed" && style.position !== "absolute") return false;
    if (!(Number(style.zIndex) >= 100000)) return false;
    const box = el.getBoundingClientRect();
    if (box.width * box.height < innerWidth * innerHeight * 0.2) return false;
    const frames = el.tagName === "IFRAME" ? [el] : [...el.querySelectorAll("iframe")];
    if (frames.some((f) => CAPTCHA.test(f.src) || CAPTCHA.test(f.title))) return false;
    if (frames.length) return true;
    // A sheet with nothing on it is there to catch clicks.
    return !el.textContent.trim() && !el.querySelector("img, video, canvas, input, button, a");
  };
  const sweep = () => {
    for (const el of document.querySelectorAll("body *")) {
      if (el.isConnected && isAd(el)) el.remove();
    }
  };
  let pending = false;
  const soon = () => {
    if (pending) return;
    pending = true;
    setTimeout(() => {
      pending = false;
      sweep();
    }, 200);
  };
  addEventListener("DOMContentLoaded", () => {
    sweep();
    new MutationObserver(soon).observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["style", "class"],
    });
  });
})();
