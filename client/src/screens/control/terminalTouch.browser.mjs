// SPDX-License-Identifier: Apache-2.0
// On a phone the terminal's history is read by swiping its output, and on a
// phone on its side Key mode, Ctrl+C, Detach and Close stay on screen. jsdom
// has no layout or touch scrolling; this sends real touches through Chromium.
// node src/screens/control/terminalTouch.browser.mjs http://127.0.0.1:5960
import { chromium } from "playwright";
import assert from "node:assert/strict";

const [url] = process.argv.slice(2);
assert(url, "Provide a simulated app URL");
const TERMINAL = "0192f7a1-0000-7000-8000-000000000a01";

async function open(browser, viewport) {
  const context = await browser.newContext({ viewport, isMobile: true, hasTouch: true, deviceScaleFactor: 2, reducedMotion: "reduce" });
  const page = await context.newPage(), errors = [];
  page.on("pageerror", e => errors.push(String(e)));
  await page.goto(url, { waitUntil: "networkidle" });
  await page.waitForTimeout(800);
  await page.evaluate((terminalId) => {
    const el = document.querySelector(".app");
    for (let f = el[Object.keys(el).find(k => k.startsWith("__reactFiber"))]; f; f = f.return)
      if (f.memoizedProps?.app) { f.memoizedProps.app.ui.push({ screen: "terminal", terminalId }, "control"); return; }
  }, TERMINAL);
  await page.locator(".terminal-output .xterm-rows").filter({ hasText: "❯" }).waitFor();
  const cdp = await context.newCDPSession(page);
  const swipe = async (from, to) => {
    const box = await page.locator(".terminal-output").boundingBox();
    const x = box.x + box.width / 2, y = box.y + box.height / 2;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y: y + from }] });
    for (let i = 1; i <= 12; i++)
      await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x, y: y + from + (to - from) * i / 12 }] });
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await page.waitForTimeout(300);
  };
  return { context, page, errors, swipe };
}

/** Where the control named `name` (a pattern) is: on screen and first under a tap at its centre, or why not. */
const reach = (page, name) => page.evaluate((name) => {
  const named = new RegExp(`^(${name})$`);
  const all = [...document.querySelectorAll("button, [role=radio]")]
    .filter(e => named.test(e.getAttribute("aria-label") ?? e.textContent.trim()));
  if (all.length !== 1) return `${all.length} controls`;
  const el = all[0];
  // A row of keys scrolls sideways on its own; bring the key into its row only.
  const row = el.closest("[data-scroll-x]");
  if (row) row.scrollLeft += el.getBoundingClientRect().left - row.getBoundingClientRect().left;
  const r = el.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2;
  if (!r.width || y < 0 || y > innerHeight || x < 0 || x > innerWidth) return `off screen at ${Math.round(x)},${Math.round(y)}`;
  const top = document.elementFromPoint(x, y);
  return el.contains(top) ? "ok" : `under ${top?.className}`;
}, name);

/** The line numbers of `line N` output the terminal is showing. */
const shown = (page) => page.evaluate(() =>
  [...document.querySelectorAll(".terminal-output .xterm-rows > div")]
    .map(row => /line (\d+)$/.exec(row.textContent.trim())?.[1]).filter(Boolean).map(Number));

const browser = await chromium.launch({ headless: true });
try {
  for (const viewport of [{ width: 844, height: 390 }, { width: 740, height: 340 }, { width: 640, height: 320 }]) {
    const { context, page, errors } = await open(browser, viewport);
    const size = `${viewport.width}x${viewport.height}`;
    const body = () => page.evaluate(() => document.querySelector(".terminal-screen .screen__body").scrollTop);
    for (const name of ["Keys|Key mode", "Detach", "Close terminal"]) assert.equal(await reach(page, name), "ok", `${size}: ${name}`);
    await page.getByRole("radio", { name: /^Key/ }).click();
    for (const name of ["Text|Text mode", "Ctrl\\+C", "Detach", "Close terminal"]) assert.equal(await reach(page, name), "ok", `${size} in Key mode: ${name}`);
    assert.equal(await body(), 0, `${size}: nothing was reached by scrolling the page`);
    await page.getByRole("button", { name: "Close terminal" }).click();
    await page.getByRole("button", { name: "Keep terminal" }).click();
    await page.getByRole("button", { name: "Detach" }).click();
    assert.equal(await reach(page, "Attach again"), "ok", `${size}: Attach again`);
    assert.deepEqual(errors, []);
    await context.close();
  }

  for (const viewport of [{ width: 390, height: 844 }, { width: 844, height: 390 }]) {
    const { context, page, errors, swipe } = await open(browser, viewport);
    const size = `${viewport.width}x${viewport.height}`;
    const field = page.getByRole("textbox", { name: /^Command for / });
    // The simulated shell answers after 100 ms; one command at a time.
    for (let n = 1; n <= 60; n++) { await field.fill(`echo line ${n}`); await field.press("Enter"); await page.waitForTimeout(130); }
    const body = () => page.evaluate(() => document.querySelector(".terminal-screen .screen__body").scrollTop);
    const latest = await shown(page), page0 = await body();
    assert(latest.includes(60), `${size}: the latest output is shown first`);

    // Finger down: earlier output comes into view and the page stays put.
    await swipe(-80, 80);
    const earlier = await shown(page);
    assert(Math.min(...earlier) < Math.min(...latest), `${size}: a swipe down shows earlier output (${latest} → ${earlier})`);
    assert.equal(await body(), page0, `${size}: the swipe scrolled the history, not the page`);
    await page.getByRole("button", { name: "Jump to latest" }).waitFor();

    // Finger up goes back towards the latest output.
    await swipe(80, -80);
    assert(Math.min(...await shown(page)) > Math.min(...earlier), `${size}: a swipe up shows later output`);

    // At the latest output a swipe up is the page's, for what lies below.
    await page.getByRole("button", { name: "Jump to latest" }).click().catch(() => {});
    await page.waitForTimeout(200);
    if (await page.evaluate(() => { const b = document.querySelector(".terminal-screen .screen__body"); return b.scrollHeight > b.clientHeight + 1; })) {
      await swipe(80, -80);
      assert((await body()) > page0, `${size}: at the latest output the page scrolls instead`);
      assert((await shown(page)).includes(60), `${size}: the history stays at the latest output`);
    }
    assert.deepEqual(errors, []);
    await context.close();
  }
  console.log("terminal history swipes and landscape controls are reachable");
} finally {
  await browser.close();
}
