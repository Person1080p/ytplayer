// iOS home-screen apps with a translucent status bar (index.html) lay fixed elements out in a
// viewport that ends short of the screen's bottom edge by about the top safe area, though the page
// itself is drawn to the edge: the tab bar, player and full-screen views float above a strip of
// background. `--vp-gap` is that shortfall, measured, and the fixed elements reach down by it
// (styles.css). Elsewhere, or once iOS gets it right, it's 0.

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
}

export function watchViewport() {
  update();
  window.addEventListener("resize", update);
  window.addEventListener("orientationchange", () => setTimeout(update, 300));
}
