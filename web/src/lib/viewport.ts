// iOS home-screen apps with a translucent status bar (index.html) lay fixed elements out in a
// viewport that ends short of the screen's bottom edge by about the top safe area, and clip them
// there, though the page canvas is drawn to the edge. `--vp-gap` is that shortfall, measured: the
// strip covers the home indicator, so the tab bar drops its own inset for it, and the strip gets
// the bar's colour (`vp-short`, styles.css). Elsewhere, or once iOS gets it right, it's 0.

const iosStandalone = (navigator as Navigator & { standalone?: boolean }).standalone === true;

function update() {
  let gap = 0;
  if (iosStandalone) {
    // iOS reports the screen in portrait whatever the orientation.
    const landscape = window.innerWidth > window.innerHeight;
    const full = landscape ? Math.min(screen.width, screen.height) : Math.max(screen.width, screen.height);
    const short = full - window.innerHeight;
    if (short > 0 && short <= 100) gap = short;
  }
  document.documentElement.style.setProperty("--vp-gap", `${gap}px`);
  document.documentElement.classList.toggle("vp-short", gap > 0);
}

export function watchViewport() {
  update();
  window.addEventListener("resize", update);
  window.addEventListener("orientationchange", () => setTimeout(update, 300));
}
