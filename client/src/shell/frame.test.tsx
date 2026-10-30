// SPDX-License-Identifier: Apache-2.0
//
// Once, on a phone, after continuing a task and choosing Latest, the whole app
// sat half a screen too high: header and conversation above the top, the
// navigation halfway down and a blank band below it. A swipe put it back. The
// page and the app's frame were clipped but still scrollable by the browser
// itself, which moves them to reveal a focused control or to settle after the
// keyboard closes, and a moved frame looks exactly like that. Neither can move
// now, and every screen that needs to scroll does it inside itself.
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { holdFrame } from "./frame";
import { Shell } from "./Shell";

const page = () => document.scrollingElement ?? document.documentElement;
const moved = (to: number) => {
  page().scrollTop = to;
  page().scrollLeft = to / 2;
};

afterEach(() => {
  moved(0);
});

it("puts the page back wherever the browser moves it, until stopped", () => {
  moved(508);
  const stop = holdFrame();
  expect(page().scrollTop).toBe(0);

  for (const event of ["scroll", "resize"]) {
    moved(508);
    window.dispatchEvent(new Event(event));
    expect(page().scrollTop).toBe(0);
    expect(page().scrollLeft).toBe(0);
  }

  stop();
  moved(508);
  window.dispatchEvent(new Event("scroll"));
  expect(page().scrollTop).toBe(508);
});

it("holds the page while the app is shown", async () => {
  localStorage.clear();
  vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  const app: App = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  try {
    session.start();
    await vi.advanceTimersByTimeAsync(900);
    render(<AppProvider app={app}><Shell /></AppProvider>);
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });

    moved(508);
    window.dispatchEvent(new Event("resize"));
    expect(page().scrollTop).toBe(0);

    cleanup();
    moved(508);
    window.dispatchEvent(new Event("scroll"));
    expect(page().scrollTop).toBe(508);
  } finally {
    cleanup();
    session.stop();
    window.__sim?.host.dispose();
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  }
});

it("gives the page and the app's frame nothing to scroll, and the screens outside the frame their own scrolling", async () => {
  // Vitest empties CSS imports, so the stylesheets are read from disk.
  const fs = await vi.importActual<{ readFileSync(path: string, encoding: "utf8"): string }>("node:fs");
  const rule = (file: string, selector: string) => {
    const css = fs.readFileSync(`src/${file}`, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const at = css.search(new RegExp(`(^|\\n|\\})\\s*${selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\{`));
    expect(at, `${selector} in ${file}`).toBeGreaterThanOrEqual(0);
    const open = css.indexOf("{", at + 1);
    return css.slice(open + 1, css.indexOf("}", open)).replace(/\s+/g, " ");
  };
  // `clip`, unlike `hidden`, cannot be scrolled even by the browser; `hidden`
  // stays first for engines without it.
  expect(rule("shell/shell.css", ".app")).toMatch(/overflow: hidden; overflow: clip;/);
  expect(rule("platform/lifecycle.css", ".connected-frame")).toMatch(/overflow: hidden; overflow: clip;/);
  expect(rule("design/base.css", "html")).toMatch(/overflow: hidden;/);

  // Pairing, below any banner, fills what is left and scrolls itself.
  expect(rule("platform/lifecycle.css", ".connected-frame > :is(.app, .welcome)")).toMatch(/flex: 1; height: auto; min-height: 0;/);
  expect(rule("screens/welcome/welcome.css", ".welcome")).toMatch(/overflow-y: auto;/);
  // So does the lock screen, whose text at a large size once relied on the page.
  const lock = rule("platform/lifecycle.css", ".local-lock");
  expect(lock).toMatch(/height: 100dvh;/);
  expect(lock).toMatch(/overflow-y: auto;/);
  expect(lock).not.toMatch(/min-height/);
  expect(rule("platform/lifecycle.css", ".local-lock__content")).toMatch(/margin: auto;/);
  // And the screen shown when the app can't start, with Try again below its error.
  const boot = rule("shell/shell.css", ".boot");
  expect(boot).toMatch(/place-content: safe center;/);
  expect(boot).toMatch(/overflow-y: auto;/);
});
