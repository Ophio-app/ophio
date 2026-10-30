// SPDX-License-Identifier: Apache-2.0
// Headless simulator only; never opens the owner's directories or devices.
// node scripts/project-folders-check.mjs http://127.0.0.1:5967 <evidence-directory>
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { chromium } from "playwright";

const [base, out] = process.argv.slice(2);
assert(base && out && new URL(base).hostname === "127.0.0.1", "Use an isolated loopback simulator");
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ headless: true });
const results = [];
try {
  for (const sample of [
    { name: "phone", width: 390, height: 844 },
    { name: "keyboard-height", width: 390, height: 430 },
    { name: "large-text", width: 360, height: 780, text: 2 },
    { name: "tablet", width: 800, height: 1100 },
  ]) {
    const context = await browser.newContext({ viewport: { width: sample.width, height: sample.height }, reducedMotion: "reduce", hasTouch: true });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", e => errors.push(e.message));
    try {
      await page.goto(base, { waitUntil: "networkidle" });
      await page.waitForFunction(() => !!window.__sim);
      await page.getByRole("button", { name: /^Work(?:,|$)/ }).click();
      await page.getByRole("button", { name: "Project Settings", exact: true }).click();
      await page.getByRole("button", { name: /^Add project/ }).click();
      const sheet = page.getByRole("dialog", { name: "Add project", exact: true });
      if (sample.text) await page.evaluate(text => { document.documentElement.style.fontSize = `${text * 100}%`; }, sample.text);
      await sheet.getByRole("button", { name: "Open Projects", exact: true }).click();
      await sheet.getByRole("button", { name: "Open weather-station", exact: true }).waitFor();
      assert(await sheet.getByRole("checkbox", { name: "Select field-notes", exact: true }).isDisabled());
      assert.equal(await sheet.getByRole("textbox", { name: "Folder on computer" }).count(), 0);
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No page overflow");
      assert(await sheet.locator(".sheet__body").evaluate(e => e.scrollWidth <= e.clientWidth + 1), "No picker overflow");
      await page.screenshot({ path: join(out, `${sample.name}-folders.png`) });
      await sheet.getByRole("checkbox", { name: "Select weather-station", exact: true }).check();
      await sheet.getByRole("checkbox", { name: "Select weekend-site", exact: true }).check();
      await sheet.getByRole("button", { name: "Review selection (2)", exact: true }).click();
      await sheet.locator(".project-folder-review li").filter({ hasText: "/home/sam/Projects/weather-station" }).waitFor();
      const add = sheet.getByRole("button", { name: "Add 2 projects", exact: true });
      await add.scrollIntoViewIfNeeded();
      const box = await add.boundingBox();
      assert(box && box.y >= 0 && box.y + box.height <= sample.height, "Confirmation reachable");
      assert(await sheet.locator(".sheet__body").evaluate(e => e.scrollWidth <= e.clientWidth + 1), "No review overflow");
      await page.screenshot({ path: join(out, `${sample.name}-review.png`) });
      await add.click();
      await sheet.waitFor({ state: "hidden" });
      assert.equal(await page.evaluate(() => window.__sim.host.projects.filter(p => ["weather-station", "weekend-site"].includes(p.name)).length), 2);
      assert.deepEqual(errors, []);
      results.push({ ...sample, passed: true });
    } catch (e) {
      await page.screenshot({ path: join(out, `${sample.name}-failed.png`) });
      writeFileSync(join(out, `${sample.name}-failed.txt`), await page.locator("body").innerText());
      throw e;
    } finally { await context.close(); }
  }
} catch (e) {
  results.push({ passed: false, error: e.stack });
  throw e;
} finally {
  await browser.close();
  writeFileSync(join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
}
console.log(`Passed ${results.length} project-folder browser scenarios`);
