// iOS home-screen apps with a translucent status bar (index.html) lay fixed elements out in a
// viewport that ends short of the screen's bottom edge by about the top safe area, and clip them
// there, though the page canvas is drawn to the edge. `--vp-gap` is that shortfall, measured: the
// strip covers the home indicator, so the tab bar drops its own inset for it, and the strip gets
// the bar's colour (`vp-short`, styles.css). Elsewhere, or once iOS gets it right, it's 0.

const iosStandalone = (navigator as Navigator & { standalone?: boolean }).standalone === true;
/**
 * The shortfall comes with a translucent status bar (the page drawn under it). With a solid one
 * the window is legitimately shorter than the screen by the status bar, which isn't the bug.
 * iOS keeps the status bar style the app was installed with, whatever index.html says now, so
 * this looks at what it does: under a translucent bar the page has a top safe-area inset.
 */
function translucentBar(): boolean {
  const probe = document.createElement("div");
  probe.style.cssText = "position:fixed;top:0;left:0;visibility:hidden;pointer-events:none;padding-top:env(safe-area-inset-top,0px)";
  document.body.appendChild(probe);
  const inset = parseFloat(getComputedStyle(probe).paddingTop) || 0;
  probe.remove();
  return inset > 0;
}

function update() {
  let gap = 0;
  // Portrait only: in landscape the top inset is zero either way.
  const portrait = window.innerHeight > window.innerWidth;
  if (iosStandalone && (!portrait || translucentBar())) {
    // iOS reports the screen in portrait whatever the orientation.
    const landscape = window.innerWidth > window.innerHeight;
    const full = landscape ? Math.min(screen.width, screen.height) : Math.max(screen.width, screen.height);
    const short = full - window.innerHeight;
    if (short > 0 && short <= 100) gap = short;
  }
  document.documentElement.style.setProperty("--vp-gap", `${gap}px`);
  document.documentElement.classList.toggle("vp-short", gap > 0);
}

/**
 * Zooming makes iOS lay the viewport out again, which ends the shortfall (zooming in by hand made
 * the tab bar fit). With zoom off, do the same once, invisibly: scale by 0.1 % and back, then
 * measure again. If the shortfall stays, nothing else changes.
 */
let nudged = false;
function nudge() {
  const meta = document.querySelector<HTMLMetaElement>('meta[name="viewport"]');
  if (nudged || !meta || !document.documentElement.classList.contains("vp-short")) return;
  nudged = true;
  const content = meta.content;
  meta.content = content.replace("initial-scale=1,", "initial-scale=1.001,").replace("maximum-scale=1,", "maximum-scale=1.001,");
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      meta.content = content;
      setTimeout(update, 300);
    }),
  );
}

export function watchViewport() {
  update();
  window.addEventListener("resize", update);
  window.addEventListener("orientationchange", () => setTimeout(update, 300));
  if (iosStandalone) setTimeout(nudge, 500);
  // Safari's own pinch events: iOS can ignore `user-scalable=no`.
  for (const type of ["gesturestart", "gesturechange"]) {
    document.addEventListener(type, (e) => e.preventDefault(), { passive: false });
  }
}
