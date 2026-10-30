// SPDX-License-Identifier: Apache-2.0
//
// Headless screenshots of the simulated client, for design review and, with
// --strict, as a pass/fail check (scripts/smoke.mjs runs it that way).
//
//   node scripts/shots.mjs [--strict] <base-url> <out-dir> <plan.json>
//
// plan.json is an array of shots:
//   { "name": "welcome-dark", "path": "/?unpaired", "width": 390, "height": 844,
//     "dark": true, "text": 2, "steps": [["click", "Pair a computer"], ["wait", 300]] }
// Optional "init" JavaScript runs before the app, after fresh storage setup,
// for startup faults such as a failed device-storage read.
//
// Steps: ["click", text] clicks the first visible button/link/tab/radio whose
// accessible name contains text; ["click", text, {"exact": true, "in": scope,
// "role": role}] matches the whole name ("starts": true its beginning), inside
// a scope ("dialog" is the top open dialog, "nav" the main navigation,
// anything else a CSS selector) or with one role; ["nav", tab] opens a main
// navigation tab; ["clickSel", css]; ["fill", label, value]; ["type", text];
// ["press", key]; ["wait", ms]; ["eval", js]; ["log", js] prints its result;
// ["height", px] shrinks the viewport as an on-screen keyboard would;
// ["shot", suffix] takes an extra screenshot mid-sequence; ["aria", css]
// prints an accessibility snapshot, for writing plans.
//
// Assertions fail the shot: ["expect", text, opts] waits for visible text
// (opts as for click, plus "timeout" in ms); ["expectRole", role, name, opts]
// for a visible element with exactly that role and name; ["expectNot", text,
// opts] for no visible text; ["reach", role, name, opts] scrolls a control
// into view and checks it is inside the viewport and not covered by anything
// else; ["inView", role, name, opts] checks the same without scrolling, for
// what must be on the first screen; ["flips", role, name, opts] taps a switch,
// checkbox or radio and checks it shows the new choice within "within" ms
// (100 by default) and stays usable, after setting the simulator's
// "latency" in ms when given.
//
// A thrown step, an unknown step or a failed assertion fails the shot. Console
// errors, page errors and content wider than the screen (outside the known
// sideways scrollers) are reported, and fail the shot under --strict unless
// the shot lists them: "allowErrors": [regex, ...], "allowOverflow": true.
// So is a screen without exactly one h1 a screen reader can reach (none is
// right while a sheet or the opened safety rail covers it); that fails it
// under --strict.
// The exit status is 1 when any shot failed.

import { chromium } from "playwright";
import { mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const args = process.argv.slice(2);
const strict = args.includes("--strict");
const [base, out, planPath] = args.filter((a) => a !== "--strict");
if (!base || !out || !planPath) {
  console.error("usage: shots.mjs [--strict] <base-url> <out-dir> <plan.json>");
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const plan = JSON.parse(readFileSync(planPath, "utf8"));

const WAIT = 4000;
const LOAD = 15000;
const ROLES = ["button", "link", "tab", "menuitem", "option", "radio"];
const escape = (t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

// Where a step looks: the whole page, the main navigation, the top open
// dialog or a CSS selector.
function scope(page, where) {
  if (!where) return page;
  if (where === "nav") return page.getByRole("navigation", { name: "Main", exact: true });
  if (where === "dialog") return page.getByRole("dialog").filter({ visible: true }).last();
  return page.locator(where).filter({ visible: true }).first();
}

function control(page, name, opts = {}) {
  const root = scope(page, opts.in);
  let loc = null;
  const match = opts.starts ? new RegExp(`^${escape(name)}`) : name;
  for (const role of opts.role ? [opts.role] : ROLES) {
    const one = root.getByRole(role, { name: match, exact: !!opts.exact });
    loc = loc ? loc.or(one) : one;
  }
  return loc.filter({ visible: true }).first();
}

function text(page, t, opts = {}) {
  return scope(page, opts.in).getByText(t, { exact: !!opts.exact }).filter({ visible: true });
}

// A control the person can tap: inside the screen and not covered by anything
// else. Scrolled to first unless it must be there as the screen opens.
async function reach(loc, scroll = true) {
  if (scroll) await loc.scrollIntoViewIfNeeded({ timeout: WAIT });
  else await loc.waitFor({ state: "visible", timeout: WAIT });
  const problem = await loc.evaluate((el) => {
    const r = el.getBoundingClientRect();
    const w = document.documentElement.clientWidth, h = window.innerHeight;
    if (r.width === 0 || r.height === 0) return "it has no size";
    if (r.left < -1 || r.top < -1 || r.right > w + 1 || r.bottom > h + 1)
      return `it lies outside the screen (${Math.round(r.left)},${Math.round(r.top)}–${Math.round(r.right)},${Math.round(r.bottom)} of ${w}×${h})`;
    const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    if (hit && hit !== el && !el.contains(hit))
      return `${hit.tagName.toLowerCase()}.${[...hit.classList].join(".")} covers it`;
    return null;
  });
  if (problem) throw new Error(`cannot reach it: ${problem}`);
}

// Taps a switch, checkbox or radio and checks it shows the new choice within
// `within` ms and is still usable, whatever the link is doing.
async function flips(loc, within) {
  await loc.scrollIntoViewIfNeeded({ timeout: WAIT });
  const problem = await loc.evaluate(async (el, ms) => {
    const state = () =>
      el.getAttribute("aria-checked") ?? el.getAttribute("aria-pressed") ?? ("checked" in el ? String(el.checked) : null);
    const was = state();
    if (was === null) return "it has no on or off state";
    el.click();
    await new Promise((r) => setTimeout(r, ms));
    if (state() === was) return `it still showed ${was} ${ms} ms after the tap`;
    if (el.disabled || el.getAttribute("aria-disabled") === "true") return "it was disabled after the tap";
    return null;
  }, within);
  if (problem) throw new Error(`toggle didn't flip: ${problem}`);
}

const browser = await chromium.launch({ headless: true });
let failures = 0;
for (const shot of plan) {
  const ctx = await browser.newContext({
    viewport: { width: shot.width ?? 390, height: shot.height ?? 844 },
    deviceScaleFactor: shot.scale ?? 2,
    colorScheme: shot.dark ? "dark" : "light",
    reducedMotion: "reduce",
    hasTouch: true,
    isMobile: (shot.width ?? 390) < 700,
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  page.on("pageerror", (e) => errors.push(String(e)));
  if (shot.fresh !== false) {
    await page.addInitScript(() => {
      if (!sessionStorage.getItem("__shots")) {
        localStorage.clear();
        sessionStorage.setItem("__shots", "1");
      }
    });
  }
  if (shot.text && shot.text !== 1) {
    await page.addInitScript((t) => {
      document.addEventListener("DOMContentLoaded", () => {
        document.documentElement.style.fontSize = `${t * 100}%`;
      });
    }, shot.text);
  }
  if (shot.init) await page.addInitScript(shot.init);
  try {
    await page.goto(base + (shot.path ?? "/"), { waitUntil: "networkidle" });
    await page.waitForTimeout(shot.settle ?? 400);
    for (const step of shot.steps ?? []) {
      const [kind, a, b, c] = step;
      const where = (o) => (o && typeof o === "object" ? o : {});
      try {
        if (kind === "click") {
          await control(page, a, where(b)).click({ timeout: WAIT });
        } else if (kind === "nav") {
          // A tab's name can carry its status: "Work, 1 needs attention".
          const name = new RegExp(`^${escape(a)}(,|$)`);
          await scope(page, "nav").getByRole("button", { name }).click({ timeout: WAIT });
        } else if (kind === "clickSel") {
          await page.locator(a).filter({ visible: true }).first().click({ timeout: WAIT });
        } else if (kind === "fill") {
          await page.getByLabel(a).filter({ visible: true }).first().fill(b, { timeout: WAIT });
        } else if (kind === "type") {
          await page.keyboard.type(a);
        } else if (kind === "press") {
          await page.keyboard.press(a);
        } else if (kind === "wait") {
          await page.waitForTimeout(a);
        } else if (kind === "eval") {
          await page.evaluate(a);
        } else if (kind === "log") {
          console.log(`     ${shot.name}: ${JSON.stringify(await page.evaluate(a))}`);
        } else if (kind === "aria") {
          console.log(await page.locator(a ?? "body").first().ariaSnapshot());
        } else if (kind === "height") {
          await page.setViewportSize({ width: page.viewportSize().width, height: a });
        } else if (kind === "shot") {
          await page.screenshot({ path: join(out, `${shot.name}-${a}.png`), fullPage: false });
        } else if (kind === "expect") {
          await text(page, a, where(b)).first().waitFor({ state: "visible", timeout: where(b).timeout ?? WAIT });
        } else if (kind === "expectRole") {
          const opts = where(c);
          await scope(page, opts.in).getByRole(a, { name: b, exact: opts.exact ?? true })
            .filter({ visible: true }).first().waitFor({ state: "visible", timeout: opts.timeout ?? WAIT });
        } else if (kind === "expectNot") {
          const n = await text(page, a, where(b)).count();
          if (n) throw new Error(`${n} visible`);
        } else if (kind === "reach") {
          await reach(control(page, b, { ...where(c), role: a, exact: where(c).exact ?? true }));
        } else if (kind === "inView") {
          await reach(control(page, b, { ...where(c), role: a, exact: where(c).exact ?? true }), false);
        } else if (kind === "flips") {
          const opts = where(c);
          if (opts.latency !== undefined) await page.evaluate((ms) => window.__sim.latency(ms), opts.latency);
          await flips(control(page, b, { ...opts, role: a, exact: opts.exact ?? true }), opts.within ?? 100);
        } else {
          throw new Error("unknown step");
        }
      } catch (e) {
        throw new Error(`step ${JSON.stringify(step)}: ${String(e.message ?? e).split("\n")[0]}`);
      }
      await page.waitForTimeout(120);
    }
    // A screen still loading shows only its outline; the shot and the checks
    // are of the screen. One that never loads fails them below. The smoke runs
    // every size at once against a dev server compiling each screen on first
    // use, so a cold one can take several seconds.
    await page.waitForFunction(() => ![...document.querySelectorAll(".screen__loading")].some((e) => e.checkVisibility({ visibilityProperty: true })), null, { timeout: LOAD }).catch(() => {});
    await page.screenshot({ path: join(out, `${shot.name}.png`), fullPage: !!shot.full });
    const overflow = await page.evaluate(() => {
      const w = document.documentElement.clientWidth;
      const bad = [];
      for (const el of document.querySelectorAll("body *")) {
        const r = el.getBoundingClientRect();
        if (r.width > 0 && r.right > w + 1 && getComputedStyle(el).visibility !== "hidden") {
          const inScroller = el.closest("[data-scroll-x], .cmd, pre, .xterm, .cm-editor, .chips");
          if (!inScroller) bad.push(`${el.tagName.toLowerCase()}.${[...el.classList].join(".")}`);
        }
      }
      return [...new Set(bad)].slice(0, 6);
    });
    // TalkBack reads each h1 as the page's title: one per screen, or none
    // while a sheet or the opened safety rail covers it.
    const titles = await page.getByRole("heading", { level: 1 }).allTextContents();
    const covered = await page.evaluate(() => !!document.querySelector("dialog:modal")
      || [...document.querySelectorAll("h1")].some((h) => !h.closest("[inert]") && getComputedStyle(h).visibility === "hidden"));
    const misTitled = titles.length === 1 || (covered && !titles.length) ? "" : `${titles.length} page titles${titles.length ? ` (${titles.join(" | ")})` : ""}`;
    const allowed = (shot.allowErrors ?? []).map((r) => new RegExp(r));
    const unexpected = errors.filter((e) => !allowed.some((r) => r.test(e)));
    const note = [unexpected.length ? `errors: ${unexpected.join(" | ")}` : "", overflow.length ? `overflow: ${overflow.join(", ")}` : "", misTitled]
      .filter(Boolean)
      .join("; ");
    const failed = strict && (unexpected.length > 0 || (overflow.length > 0 && !shot.allowOverflow) || !!misTitled);
    if (failed) failures++;
    console.log(`${failed ? "FAIL" : note ? "WARN" : "ok  "} ${shot.name}${note ? " — " + note : ""}`);
    if (failed) await page.screenshot({ path: join(out, `${shot.name}-fail.png`) }).catch(() => {});
  } catch (e) {
    failures++;
    console.log(`FAIL ${shot.name} — ${String(e).split("\n")[0]}`);
    await page.screenshot({ path: join(out, `${shot.name}-fail.png`) }).catch(() => {});
  }
  await ctx.close();
}
await browser.close();
process.exit(failures ? 1 : 0);
