// SPDX-License-Identifier: Apache-2.0
// Headless simulator-only design checks; never pairs with a real computer.
// node scripts/fruiting-check.mjs http://127.0.0.1:5962 <evidence-directory>
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright";

const [base, out] = process.argv.slice(2);
assert(base && out && new URL(base).hostname === "127.0.0.1", "Use an isolated loopback simulator");
mkdirSync(out, { recursive: true });
// Every strand colour in the generated ground, for worst-case text contrast.
const strands = Object.fromEntries(["dark", "light"].map(theme => [theme,
  [...new Set(readFileSync(new URL(`../src/design/assets/mat-${theme}.svg`, import.meta.url), "utf8").match(/stroke="#[0-9a-f]{6}"/g).map(m => m.slice(8, -1)))]]));
const browser = await chromium.launch({ headless: true });
const report = [];
const chipEdges = [];
const cases = [
  { name: "phone-dark", width: 390, height: 844, dark: true },
  { name: "phone-light", width: 390, height: 844 },
  { name: "landscape-dark", width: 844, height: 390, dark: true },
  { name: "landscape-light", width: 844, height: 390 },
  { name: "tablet-dark", width: 1024, height: 1366, dark: true },
  { name: "tablet-light", width: 1024, height: 1366 },
  { name: "large-dark", width: 360, height: 780, text: 2, dark: true },
  { name: "large-light", width: 360, height: 780, text: 2 },
  { name: "system-reduced", width: 390, height: 844, dark: true, reduce: true },
  { name: "app-reduced", width: 390, height: 844, dark: true, appReduce: true },
];
async function open(c, path = "/") {
  const ctx = await browser.newContext({ viewport: { width: c.width, height: c.height }, colorScheme: c.dark ? "dark" : "light", reducedMotion: c.reduce ? "reduce" : "no-preference", hasTouch: true });
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", e => errors.push(e.message));
  page.on("console", m => { if (m.type() === "error") errors.push(m.text()); });
  await page.addInitScript(({ text, appReduce }) => {
    if (!sessionStorage.getItem("fruiting-check")) {
      localStorage.clear();
      localStorage.setItem("prefs", JSON.stringify({ theme: "system", reduceMotion: !!appReduce, lastProfileId: null }));
      sessionStorage.setItem("fruiting-check", "1");
    }
    document.addEventListener("DOMContentLoaded", () => { document.documentElement.style.fontSize = `${100 * (text ?? 1)}%`; });
  }, c);
  await page.goto(base + path, { waitUntil: "networkidle" });
  await page.evaluate(() => document.fonts.ready);
  await page.waitForFunction(() => !!window.__sim);
  return { ctx, page, errors };
}
async function shot(page, name) {
  await page.screenshot({ path: join(out, `${name}.png`) });
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${name}: horizontal page overflow`);
}
async function newTask(page) {
  await page.locator(".chat-head__back").click();
  await page.getByRole("button", { name: /^New/ }).first().click();
  // A project that can use GitHub asks what's new first.
  const task = page.getByRole("button", { name: /^New task/ });
  if (await task.count()) await task.first().click();
  await page.locator(".talk-new").waitFor();
}
async function noDisplay(page) {
  assert.equal(await page.locator(".fruiting, .welcome img, .talk-new img").count(), 0, "No artwork display in the working UI");
  assert.equal(await page.getByRole("button", { name: /disturb|specimen/i }).count(), 0, "Every control serves a real task");
  assert(!/room to grow|decorative organism|study in symbiosis/i.test(await page.locator("body").innerText()), "No ornamental app copy");
}
async function colors(page) {
  return page.evaluate(() => {
    const root = getComputedStyle(document.documentElement);
    const rgb = name => root.getPropertyValue(name).trim().slice(1).match(/../g).map(n => parseInt(n, 16) / 255);
    const lum = name => rgb(name).map(x => x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4).reduce((s, x, i) => s + x * [0.2126, 0.7152, 0.0722][i], 0);
    const contrast = (fg, bg) => { const a = lum(fg), b = lum(bg); return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05); };
    const pairs = [];
    for (const bg of ["--bg", "--surface", "--surface-raised", "--surface-sunken", "--surface-pressed"])
      for (const fg of ["--ink", "--ink-2", "--ink-3", "--bloom", "--accent-text"]) pairs.push({ fg, bg, ratio: contrast(fg, bg) });
    for (const [fg, bg] of [["--on-signal", "--signal"], ["--on-accent", "--accent"], ["--accent-text", "--accent-soft"], ["--bloom", "--bloom-soft"], ["--stop-text", "--stop-soft"], ["--warn-text", "--warn-soft"], ["--ok-text", "--ok-soft"]]) pairs.push({ fg, bg, ratio: contrast(fg, bg) });
    return pairs;
  });
}
async function paintedText(page) {
  return page.evaluate(strands => {
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    const rgba = css => {
      ctx.clearRect(0, 0, 1, 1);
      ctx.fillStyle = css;
      ctx.fillRect(0, 0, 1, 1);
      const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
      return [r, g, b, a / 255];
    };
    const over = (fg, bg) => fg.slice(0, 3).map((v, i) => v * fg[3] + bg[i] * (1 - fg[3])).concat(1);
    const root = getComputedStyle(document.documentElement);
    const theme = document.documentElement.dataset.theme;
    // The ground: any strand may run right under a letter, with both corner
    // glows at full strength and no veil. The bar: its gills at full strength.
    const ground = bg => [bg, ...strands[theme].map(rgba)].flatMap(b => [b, over(rgba(root.getPropertyValue("--glow-b")), over(rgba(root.getPropertyValue("--glow-a")), b))]);
    const gills = bg => [bg, over(rgba(root.getPropertyValue("--gill")), bg)];
    const lum = rgb => rgb.slice(0, 3).map(v => v / 255).map(x => x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4).reduce((s, x, i) => s + x * [0.2126, 0.7152, 0.0722][i], 0);
    const result = [];
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    while (walker.nextNode()) {
      const node = walker.currentNode, el = node.parentElement;
      if (!node.textContent.trim() || !el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) || el.closest('[aria-hidden="true"], [inert], :disabled, script, style')) continue;
      const rect = el.getBoundingClientRect();
      const x = Math.max(0, Math.min(innerWidth - 1, rect.x + rect.width / 2));
      const y = rect.y + rect.height / 2;
      if (y < 0 || y >= innerHeight || !el.contains(document.elementFromPoint(x, y))) continue;
      const chain = [];
      for (let p = el; p; p = p.parentElement) chain.unshift(p);
      let bgs = [[255, 255, 255, 1]], pressed = false;
      const textured = p => p.matches(".route, .welcome, .nav") ? false : getComputedStyle(p).backgroundImage !== "none";
      if (chain.some(p => Number(getComputedStyle(p).opacity) < 1 || textured(p))) continue;
      for (const p of chain) {
        bgs = bgs.map(bg => over(rgba(getComputedStyle(p).backgroundColor), bg));
        if (p.matches(".route, .welcome")) bgs = bgs.flatMap(ground);
        if (p.matches(".nav")) bgs = bgs.flatMap(gills);
        // Conservative solid coverage of each masked surface; real lines cover less.
        if (p.matches(".ctx, .welcome--intro, .group, .btn--primary, .nav__pill")) {
          const texture = getComputedStyle(p, "::before");
          if (texture.display !== "none" && texture.content !== "none") {
            const tint = rgba(texture.backgroundColor);
            tint[3] *= Number(texture.opacity);
            bgs = bgs.map(bg => over(tint, bg));
          }
        }
        // A press in progress: its tint at full strength everywhere.
        if (p.matches("[data-reach]")) {
          const reach = getComputedStyle(p, "::after");
          if (reach.content !== "none") { bgs = bgs.map(bg => over(rgba(reach.backgroundColor), bg)); pressed = true; }
        }
        if (p.matches(".welcome--intro")) {
          const dark = theme === "dark";
          bgs = bgs.map(bg => over(dark ? [255, 255, 255, 0.025] : [0, 0, 0, 0.025], bg));
        }
      }
      const style = getComputedStyle(el);
      const ratio = Math.min(...bgs.map(bg => {
        const fg = over(rgba(style.color), bg);
        const a = lum(fg), b = lum(bg);
        return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
      }));
      const large = parseFloat(style.fontSize) >= (Number(style.fontWeight) >= 700 ? 18.67 : 24);
      result.push({ text: node.textContent.trim().slice(0, 70), ratio, required: large ? 3 : 4.5, pressed });
    }
    return result;
  }, strands);
}
// A press grows inside the control without taking the hit or the contrast,
// then settles. Returns the text contrast measured while it was held. The
// page stops the press at a fixed point the moment it starts, so however slow
// the runner is, the press can't finish before it is measured.
async function touch(page, target, c, name) {
  const box = await target.boundingBox();
  const [x, y] = [box.x + box.width * 0.3, box.y + box.height / 2];
  await page.mouse.move(x, y);
  await page.evaluate(() => addEventListener("pointerdown", e => {
    const host = e.target.closest?.("[data-reach]");
    window.heldPress = host ? host.getAnimations({ subtree: true }) : [];
    for (const a of window.heldPress) { a.pause(); a.currentTime = 160; }
  }, { once: true }));
  await page.mouse.down();
  const during = await target.evaluate((el, [x, y]) => ({ reach: el.dataset.reach ?? null, hit: el.contains(document.elementFromPoint(x, y)), after: Number(getComputedStyle(el, "::after").opacity), stopped: window.heldPress?.length ?? 0 }), [x, y]);
  assert(during.hit, "Touch decoration never takes the press");
  if (c.reduce || c.appReduce) assert.equal(during.reach, null, "No touch growth with motion reduced");
  else {
    assert(during.reach && during.stopped > 0 && during.after > 0, "A press grows inside the control");
    await page.screenshot({ path: join(out, `${name}.png`) });
  }
  const held = await checkPaint(page);
  if (!c.reduce && !c.appReduce) {
    assert(held.some(s => s.pressed), "The held press was measured under the text it covers");
    await page.evaluate(() => window.heldPress.forEach(a => a.play()));
  }
  // Let go away from the control, so the check stays on this screen.
  await page.mouse.move(2, 2);
  await page.mouse.up();
  await page.waitForTimeout(900);
  const after = await target.evaluate(el => ({ reach: el.dataset.reach ?? null, running: el.getAnimations({ subtree: true }).filter(a => a.playState === "running").length }));
  assert.deepEqual(after, { reach: null, running: 0 }, "A press settles");
  return held;
}
// A press on a consent or emergency control leaves it untouched.
const pressedStill = el => {
  const box = el.getBoundingClientRect();
  el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, isPrimary: true, button: 0, clientX: box.x + 4, clientY: box.y + 4 }));
  return el.dataset.reach ?? null;
};
async function checkPaint(page) {
  const samples = await paintedText(page);
  assert(samples.length > 0, "Expected visible text contrast samples");
  assert.deepEqual(samples.filter(p => p.ratio < p.required), [], "Composed text contrast");
  return samples;
}
try {
  for (const c of cases) {
    const painted = [];
    let run = await open(c, "/?unpaired");
    try {
      const { page, errors } = run;
      await shot(page, `${c.name}-welcome`);
      painted.push(...await checkPaint(page));
      await noDisplay(page);
      const pair = page.getByRole("button", { name: "Pair a computer", exact: true });
      assert((await pair.boundingBox()).height >= 48);
      assert.equal(await pair.evaluate(el => getComputedStyle(el, "::before").pointerEvents), "none");
      painted.push(...await touch(page, pair, c, `${c.name}-pair-held`));
      await pair.click();
      if (!c.reduce && !c.appReduce) {
        await page.waitForTimeout(160);
        await shot(page, `${c.name}-release`);
      }
      await page.getByRole("heading", { name: "Pair with your computer", exact: true }).waitFor();
      await noDisplay(page);
      assert.equal(await page.locator(".welcome").evaluate(el => getComputedStyle(el, "::before").content), "none", "Pairing remains undecorated");
      await shot(page, `${c.name}-pairing`);
      await page.waitForTimeout(1000);
      const cloud = await page.evaluate(() => {
        const r = document.querySelector(".release");
        return r && { open: r.matches(":popover-open"), running: r.getAnimations().length, pointer: getComputedStyle(r).pointerEvents, hidden: r.getAttribute("aria-hidden") };
      });
      if (c.reduce || c.appReduce) assert.equal(cloud, null, "Nothing is released with motion reduced");
      else assert.deepEqual(cloud, { open: false, running: 0, pointer: "none", hidden: "true" }, "Released specks settle and leave");
      assert.deepEqual(errors, []);
    } finally { await run.ctx.close(); }
    run = await open(c);
    try {
      const { page, errors } = run;
      await page.locator(".chat-head__main").waitFor();
      const pairs = await colors(page);
      assert.deepEqual(pairs.filter(p => p.ratio < 4.5), [], `${c.name}: text contrast`);
      await shot(page, `${c.name}-conversation`);
      assert.equal(await page.locator(".talk .fruiting").count(), 0, "No art over a conversation");
      await newTask(page);
      await shot(page, `${c.name}-new-task`);
      painted.push(...await checkPaint(page));
      const review = page.locator(".composer .composer__send:disabled");
      assert.equal(await review.count(), 1, "An empty draft can't be reviewed yet");
      // Send waits out of sight until there is something to send.
      assert.equal(await review.evaluate(el => getComputedStyle(el).visibility), "hidden", "No control shown that can't be used");
      // The chip's hit area reaches past its edge; a slow press there, as a
      // mouse makes, still lands after the press has started to grow.
      // Short windows fold the chips into Tools.
      const chip = page.locator(".composer__chip").filter({ visible: true }).first();
      if (await chip.count()) {
        const chipBox = await chip.boundingBox();
        await page.mouse.move(chipBox.x + chipBox.width / 2, chipBox.y + chipBox.height + 4);
        await page.mouse.down();
        await page.waitForTimeout(100);
        await page.mouse.up();
        await page.getByRole("dialog").filter({ visible: true }).waitFor({ timeout: 2000 });
        await page.keyboard.press("Escape");
        await page.getByRole("dialog").filter({ visible: true }).waitFor({ state: "detached", timeout: 2000 });
        chipEdges.push(c.name);
      }
      const input = page.locator(".composer textarea").filter({ visible: true });
      await input.fill("Keep this draft exactly.");
      await page.waitForTimeout(350);
      await noDisplay(page);
      assert.equal(await page.locator(".composer__bubble").filter({ visible: true }).evaluate(el => getComputedStyle(el, "::after").opacity), "0.65");
      await shot(page, `${c.name}-composer-focus`);
      await input.evaluate(el => { el.dataset.fruitingProbe = "kept"; });
      // A desktop browser does not open an IME; blur also exercises tab focus restoration.
      await input.evaluate(el => el.blur());
      for (const tab of ["Work", "Library", "Control", "Talk"]) {
        await page.locator(".nav__tab").filter({ hasText: tab }).click();
        if (c.reduce || c.appReduce) {
          assert.equal(await page.locator(".nav__tab.is-on .nav__icon").evaluate(el => getComputedStyle(el).animationName), "none");
          assert.equal(await page.locator(".nav__tab.is-on .nav__pill").evaluate(el => getComputedStyle(el, "::before").animationName), "none");
        } else {
          assert.equal(await page.locator(".nav__tab.is-on .nav__pill").evaluate(el => getComputedStyle(el, "::before").animationName), "edge-arrive");
        }
        // A screen's first visit loads it; photograph the screen, not its placeholder.
        await page.locator(".screen__loading").waitFor({ state: "detached" });
        await page.waitForTimeout(150);
        await shot(page, `${c.name}-${tab.toLowerCase()}`);
      }
      assert.equal(await input.inputValue(), "Keep this draft exactly.");
      assert.equal(await input.getAttribute("data-fruiting-probe"), "kept");
      assert.equal(await input.evaluate(el => document.activeElement === el), false, "Tab restoration must not summon the IME");
      const stop = page.locator(".app__safety :is(.btn--stop, .iconbtn--stop)").filter({ visible: true }).first();
      assert(await stop.isVisible(), "Safety remains available");
      const stopBox = await stop.boundingBox();
      assert(stopBox.y >= 0 && stopBox.y + stopBox.height <= c.height && stopBox.height >= 48, "Stop stays in the viewport");
      assert.equal(await stop.evaluate(el => getComputedStyle(el, "::before").content), "none", "No texture on Stop");
      assert.equal(await stop.evaluate(pressedStill), null, "Stop stays still under a press");
      await page.waitForTimeout(500);
      assert.equal(await page.locator(".nav").evaluate(el => el.getAnimations({ subtree: true }).filter(a => a.playState === "running").length), 0, "Navigation must settle, not keep animating");
      await page.locator(".nav__tab").filter({ hasText: "Work" }).click();
      await page.waitForTimeout(400);
      painted.push(...await touch(page, page.locator(".row--action").filter({ visible: true }).first(), c, `${c.name}-touch`));
      assert.deepEqual(errors, []);
      report.push({ name: c.name, passed: true, minimumTextContrast: Math.min(...pairs.map(p => p.ratio)), contrast: pairs, paintedText: painted });
    } finally { await run.ctx.close(); }
  }

  assert(chipEdges.length >= 4, `The chip's outer edge was pressed in ${chipEdges.join(", ") || "no case"}`);

  // Saved appearance and in-app motion preferences, exercised through real UI.
  const run = await open(cases[1]);
  try {
    const { page, errors } = run;
    await page.locator(".ctx").getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByRole("radio", { name: "Dark", exact: true }).click();
    await page.getByRole("checkbox", { name: "Reduce motion", exact: true }).check();
    assert.equal(await page.locator("html").getAttribute("data-theme"), "dark");
    assert.equal(await page.locator("html").getAttribute("data-reduce-motion"), "true");
    await shot(page, "appearance");
    await page.reload({ waitUntil: "networkidle" });
    assert.equal(await page.locator("html").getAttribute("data-theme"), "dark");
    assert.equal(await page.locator("html").getAttribute("data-reduce-motion"), "true");
    // The reload keeps Settings open over Talk.
    await page.locator(".bar").getByRole("button", { name: "Back", exact: true }).click();
    await newTask(page);
    await page.locator(".composer textarea").fill("Motion stays off while typing.");
    await page.waitForTimeout(20);
    assert.equal(await page.locator(".composer__bubble").evaluate(el => getComputedStyle(el, "::after").transform), "none");
    await page.emulateMedia({ forcedColors: "active" });
    await page.keyboard.press("Tab");
    await page.locator(".nav__tab").filter({ hasText: "Library" }).focus();
    assert.notEqual(await page.locator(".nav__tab:focus").evaluate(el => getComputedStyle(el).outlineStyle), "none");
    await shot(page, "forced-colors-focus");
    assert.deepEqual(errors, []);
    report.push({ name: "saved-appearance-motion-and-forced-colors", passed: true });
  } finally { await run.ctx.close(); }

  const run2 = await open(cases[0]);
  try {
    const { page, errors } = run2;
    await newTask(page);
    const input = page.locator(".composer textarea");
    await input.fill("Keyboard layout check");
    await page.setViewportSize({ width: 390, height: 430 });
    await page.waitForTimeout(200);
    await input.scrollIntoViewIfNeeded();
    await shot(page, "keyboard-height");
    const box = await input.boundingBox();
    assert(box.y >= 0 && box.y + box.height <= 430, "Composer remains reachable at keyboard height");
    await page.setViewportSize({ width: 390, height: 844 });
    await input.evaluate(el => el.blur());
    await page.getByRole("button", { name: /^Fix the flaky retry test/ }).click();
    await page.locator(".approval").filter({ visible: true }).waitFor();
    await page.locator(".approval").filter({ visible: true }).scrollIntoViewIfNeeded();
    await shot(page, "approval");
    assert.equal(await page.locator(".approval .btn--primary").filter({ visible: true }).evaluate(el => getComputedStyle(el, "::before").display), "none", "No texture on consent actions");
    assert.equal(await page.locator(".approval .btn--primary").filter({ visible: true }).evaluate(pressedStill), null, "Consent actions stay still under a press");
    const layered = await page.evaluate(() => {
      const d = document.createElement("dialog");
      d.innerHTML = '<button class="btn btn--primary" type="button">Probe</button>';
      document.body.append(d);
      d.showModal();
      d.querySelector("button").click();
      const r = document.querySelector(".release");
      const seen = { open: r?.matches(":popover-open") ?? false, modal: d.open };
      d.close();
      d.remove();
      return seen;
    });
    assert.deepEqual(layered, { open: true, modal: true }, "Released specks rise above an open sheet");
    assert.equal(await page.locator(".talk .fruiting").count(), 0);
    assert.deepEqual(errors, []);
    report.push({ name: "keyboard-height-and-approval", passed: true });
  } finally { await run2.ctx.close(); }
} catch (e) {
  report.push({ passed: false, error: e.stack });
  throw e;
} finally {
  writeFileSync(join(out, "results.json"), JSON.stringify(report, null, 2) + "\n");
  await browser.close();
}
console.log(`Passed ${report.length} browser scenarios; screenshots and contrast results in ${out}`);
