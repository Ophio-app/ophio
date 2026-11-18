// SPDX-License-Identifier: Apache-2.0
// Headless regression: retained handoffs must not consume the connected app.
// node scripts/handoff-cap.mjs <sim-url> <output-dir> [--expect-regression]
import { chromium } from "playwright";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [base, out] = process.argv.slice(2);
if (!base || !out) throw new Error("Supply the simulator URL and evidence directory.");
const expectRegression = process.argv.includes("--expect-regression");
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ headless: true });
const results = [];
try {
  for (const shape of [
    { name: "phone-landscape", width: 915, height: 412, split: false },
    { name: "fold-landscape", width: 1080, height: 840, split: true },
  ]) for (const text of [100, 200]) for (const warnings of [2, 3]) {
    const name = `${shape.name}-text${text}-warnings${warnings}`;
    const context = await browser.newContext({ viewport: { width: shape.width, height: shape.height }, deviceScaleFactor: 1, isMobile: true, hasTouch: true, colorScheme: "dark", reducedMotion: "reduce" });
    try {
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", error => errors.push(error.message));
      page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
      const cdp = await context.newCDPSession(page);
      if (shape.split) {
        const feature = { orientation: "vertical", offset: 540, maskLength: 0 };
        await cdp.send("Emulation.setDeviceMetricsOverride", { width: shape.width, height: shape.height, screenWidth: shape.width, screenHeight: shape.height, deviceScaleFactor: 1, mobile: true, displayFeature: feature });
        await cdp.send("Emulation.setDisplayFeaturesOverride", { features: [feature] });
      }
      await page.addInitScript(({ warnings, split }) => {
        const target = "0192f7a1-0000-7000-8000-000000000601";
        const projectId = "0192f7a1-0000-7000-8000-000000000101";
        localStorage.setItem("prefs", JSON.stringify({ theme: "dark", reduceMotion: true, lastProfileId: "sim-studio", claudeBypassPermissions: true, codexBypassPermissions: true, allProjects: false }));
        const navigation = { tab: "work", projectId, taskList: true, conversing: false, talkAll: false };
        localStorage.setItem("ui.sim-studio", JSON.stringify({ version: 2, state: navigation }));
        localStorage.setItem("ui.sim-studio.side.right", JSON.stringify({ version: 2, state: navigation }));
        localStorage.setItem("split-view.v1", JSON.stringify({ open: split, active: "left", sides: { left: { profileId: "sim-studio", all: false }, right: { profileId: "sim-studio", all: false } } }));
        // Unknown pending input stays unresolved on the two other computers.
        const ids = warnings === 3 ? ["sim-studio", "sim-spare", "sim-laptop"] : ["sim-spare", "sim-laptop"];
        localStorage.setItem("unresolved-computers.v1", JSON.stringify(ids.map((profileId, i) => ({
          profileId, since: Date.now() - 30000, busy: false, detail: "",
          risk: { leases: [], privateTargets: profileId === "sim-studio" ? [target] : [], stoppingTasks: [], pendingInput: [{ commandId: `0192f7a1-0000-7000-8000-00000000900${i}`, targetId: target }] },
        }))));
        // The current computer's real, unverified private step keeps its third
        // warning visible too; no application component or stylesheet is mocked.
        let sim;
        Object.defineProperty(window, "__sim", { configurable: true, get: () => sim, set(value) {
          sim = value;
          if (warnings === 3) { value.host.computer.unverified = true; value.host.computer.privateStep(target, true); }
        } });
      }, { warnings, split: shape.split });
      await page.goto(`${base}/?three-computers`);
      await page.evaluate(text => { document.documentElement.style.fontSize = `${16 * text / 100}px`; }, text);
      await page.waitForFunction(count => document.querySelectorAll(".split-global > .handoff-banner").length === count, warnings);
      await page.locator(".pane:not([inert]) .screen__loading").first().waitFor({ state: "hidden" });
      await page.evaluate(() => document.fonts.ready);
      await page.waitForTimeout(900);
      const capture = async suffix => {
        // Playwright's screenshot helper clears emulated display segments.
        const { data } = await cdp.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
        writeFileSync(join(out, `${name}${suffix}.png`), Buffer.from(data, "base64"));
      };
      await capture("");
      const measure = () => page.evaluate(() => {
        const box = el => { const b = el.getBoundingClientRect(); return { x: b.x, y: b.y, width: b.width, height: b.height, bottom: b.bottom }; };
        const reachable = el => {
          const b = el.getBoundingClientRect();
          const hit = document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2);
          return b.width > 0 && b.height > 0 && b.top >= 0 && b.bottom <= innerHeight + 1 && b.left >= 0 && b.right <= innerWidth + 1 && !!hit && (el === hit || el.contains(hit));
        };
        const global = document.querySelector(".split-global"), frame = document.querySelector(".split-frame");
        return {
          viewport: { width: innerWidth, height: innerHeight, fontSize: getComputedStyle(document.documentElement).fontSize },
          frame: box(frame), stack: { ...box(global), scrollHeight: global.scrollHeight, clientHeight: global.clientHeight, overflowY: getComputedStyle(global).overflowY },
          warnings: global.querySelectorAll(".handoff-banner").length,
          documentOverflow: document.documentElement.scrollWidth > innerWidth + 1,
          panes: [...document.querySelectorAll(".pane:not([inert])")].map(pane => ({
            id: pane.dataset.paneId, ...box(pane),
            main: box(pane.querySelector(".app__main")),
            stops: [...pane.querySelectorAll(".named-stop button")].filter(el => el.getBoundingClientRect().width > 0).map(el => ({ name: el.getAttribute("aria-label"), ...box(el), reachable: reachable(el) })),
          })),
          segments: window.viewport?.segments?.map(b => ({ x: b.x, y: b.y, width: b.width, height: b.height })),
        };
      });
      const initial = await measure();
      const failures = [];
      if (initial.warnings !== warnings) failures.push("The retained warning count changed.");
      if (initial.stack.height > initial.frame.height * 0.35 + 1) failures.push("Warnings exceed 35% of the connected frame.");
      if (initial.stack.overflowY !== "auto") failures.push("Warnings have no internal scrolling.");
      if (initial.documentOverflow) failures.push("The page overflows horizontally.");
      if (initial.panes.length !== (shape.split ? 2 : 1)) failures.push("The expected connected panes are not visible.");
      for (const pane of initial.panes) {
        if (pane.height <= 0 || pane.main.height <= 0) failures.push(`${pane.id} has no usable app space.`);
        if (!pane.stops.some(stop => stop.reachable)) failures.push(`${pane.id} has no reachable task Stop.`);
      }
      const actions = [];
      let expanded = null;
      // Check every retained warning's urgent action by scrolling its own stack,
      // without issuing Stop, release, Return, or dismissing any real warning.
      if (!expectRegression) {
        const banners = page.locator(".split-global > .handoff-banner");
        for (let i = 0; i < warnings; i++) {
          const banner = banners.nth(i);
          await banner.locator("summary").click();
          for (const button of await banner.locator("button").all()) {
            await button.scrollIntoViewIfNeeded();
            await button.click({ trial: true });
            actions.push(await button.textContent());
          }
        }
        await capture("-details-scrolled");
        expanded = await measure();
        if (expanded.stack.height > expanded.frame.height * 0.35 + 1) failures.push("Expanded warning details exceed the stack cap.");
        for (const pane of expanded.panes) if (!pane.stops.some(stop => stop.reachable)) failures.push(`${pane.id} lost task Stop after warning details expanded.`);
      }
      if (errors.length) failures.push(...errors);
      results.push({ name, text, expectedWarnings: warnings, errors, failures, warningActionsChecked: actions, expanded, ...initial });
      console.log(`${failures.length ? "FAIL" : "PASS"} ${name}: stack ${initial.stack.height}px, panes ${initial.panes.map(p => p.height).join(", ")}px; ${failures.join(" ")}`);
    } finally { await context.close(); }
  }
} finally {
  writeFileSync(join(out, "results.json"), JSON.stringify(results, null, 2));
  await browser.close();
}
if (expectRegression ? !results.some(row => row.failures.length) : results.some(row => row.failures.length)) process.exitCode = 1;
