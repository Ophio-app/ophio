// SPDX-License-Identifier: Apache-2.0
//
// The app fills the window and scrolls only inside its screens; the page
// itself has nothing to scroll. A browser can still move it, revealing a
// focused control or settling after the keyboard or the window changes size,
// and a moved page lifts the whole app, navigation included, over a blank
// band. The styles leave the page nothing to move; this puts it back if it
// moves anyway.

/** Keep the page at its origin while the app is shown; returns a function that stops. */
export function holdFrame(): () => void {
  const page = document.scrollingElement ?? document.documentElement;
  const hold = () => {
    if (page.scrollTop !== 0) page.scrollTop = 0;
    if (page.scrollLeft !== 0) page.scrollLeft = 0;
  };
  const vv = window.visualViewport;
  hold();
  window.addEventListener("scroll", hold, { passive: true });
  window.addEventListener("resize", hold);
  vv?.addEventListener("resize", hold);
  return () => {
    window.removeEventListener("scroll", hold);
    window.removeEventListener("resize", hold);
    vv?.removeEventListener("resize", hold);
  };
}
