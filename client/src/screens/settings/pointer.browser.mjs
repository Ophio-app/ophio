// SPDX-License-Identifier: Apache-2.0
// Real headless mouse/touch event regression, including the browser's focus change.
// node src/screens/settings/pointer.browser.mjs http://127.0.0.1:5917 /tmp/pointer-shots
import { chromium } from "playwright";
import assert from "node:assert/strict";
import { mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const [url, out] = process.argv.slice(2);
assert(url && out, "Provide a simulated app URL and screenshot directory");
mkdirSync(out, { recursive: true });
const plan = JSON.parse(readFileSync("scripts/plans/library-pairing-and-sheets.json", "utf8"));
const navigate = plan.find(s => s.name === "connections-light").steps[0][1];
const browser = await chromium.launch({ headless: true });
try {
  for (const kind of ["mouse", "touch"]) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true, reducedMotion: "reduce" });
    const page = await context.newPage(), errors = [];
    page.on("pageerror", e => errors.push(String(e)));
    page.on("console", m => { if (m.type() === "error") errors.push(m.text()); });
    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(1000);
    await page.evaluate(navigate);
    await page.waitForTimeout(1600);
    await page.getByRole("button", { name: "Rename Pixel 9", exact: true }).click();
    const field = page.getByLabel("Device name"), dialog = page.getByRole("dialog", { name: "Rename Pixel 9" });
    await field.fill("Retain this name");
    const box = await dialog.boundingBox();
    assert(box && box.y > 4, "Backdrop must be reachable");
    await page.evaluate(() => {
      window.__sheetEvents = [];
      for (const event of ["pointerdown", "focusout", "click"]) document.addEventListener(event, () => {
        window.__sheetEvents.push({ event, fieldFocused: document.activeElement?.matches("input") ?? false });
      }, true);
    });
    const tap = () => kind === "touch" ? page.touchscreen.tap(4, box.y / 2) : page.mouse.click(4, box.y / 2);
    await tap();
    assert(await dialog.isVisible(), "First outside tap must keep the sheet");
    assert.equal(await field.inputValue(), "Retain this name");
    assert.equal(await field.evaluate(e => e === document.activeElement), false);
    const events = await page.evaluate(() => window.__sheetEvents);
    assert.equal(events[0].event, "pointerdown");
    assert.equal(events[0].fieldFocused, true);
    assert(events.findIndex(e => e.event === "focusout") < events.findIndex(e => e.event === "click"), "Exercise focus loss before click");
    await page.screenshot({ path: join(out, `real-${kind}.png`) });
    await tap();
    await page.waitForTimeout(100);
    assert.equal(await dialog.count(), 0, "Second outside tap closes");
    assert.deepEqual(errors, []);
    console.log(`ok ${kind}: pointerdown → focus change → click; text retained; second tap closes`);
    await context.close();
  }
} finally {
  await browser.close();
}
