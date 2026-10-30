// SPDX-License-Identifier: Apache-2.0
// Headless simulator only; never launches native inference or uses real accounts.
// node scripts/launch-flags-check.mjs http://127.0.0.1:47864 <evidence-directory>
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
    { name: "landscape", width: 844, height: 390 },
    { name: "large-text", width: 360, height: 780, text: 2 },
  ]) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, reducedMotion: "reduce", hasTouch: true });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", e => errors.push(e.message));
    const shot = async name => {
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No horizontal page overflow");
      await page.screenshot({ path: join(out, `${sample.name}-${name}.png`) });
    };
    try {
      await page.goto(base, { waitUntil: "networkidle" });
      await page.waitForFunction(() => !!window.__sim);
      await page.locator(".chat-head__back").click();
      await page.getByRole("button", { name: /^New/ }).first().click();
      // A project that can use GitHub asks what's new first.
      const task = page.getByRole("button", { name: /^New task/ });
      if (await task.count()) await task.first().click();
      await page.locator(".composer__chip").filter({ hasText: "Claude Code" }).click();
      const sheet = page.getByRole("dialog", { name: "Agent for this task" });
      await page.setViewportSize({ width: sample.width, height: sample.height });
      if (sample.text) await page.evaluate(text => { document.documentElement.style.fontSize = `${text * 100}%`; }, sample.text);
      assert.equal(await sheet.getByRole("button", { name: "Skip permissions", exact: true }).getAttribute("aria-pressed"), "true");
      await sheet.getByRole("group", { name: "Agent", exact: true }).getByRole("button", { name: /Codex/ }).click();
      await sheet.getByRole("button", { name: "Sandbox, no prompts", exact: true }).click();
      await sheet.getByRole("button", { name: "Sandbox, no prompts", exact: true }).scrollIntoViewIfNeeded();
      assert.equal(await sheet.getByRole("button", { name: "Sandbox, no prompts", exact: true }).getAttribute("aria-pressed"), "true");
      await shot("picker");
      await sheet.getByRole("button", { name: "Use this agent", exact: true }).click();
      await page.locator(".composer textarea").fill("Check launch policy persistence");
      await page.locator(".composer__send").click();
      await page.getByRole("heading", { name: "Review new task", exact: true }).waitFor();
      const summary = page.getByText("Sandbox, no prompts · --sandbox workspace-write --ask-for-approval never", { exact: true });
      await summary.scrollIntoViewIfNeeded();
      assert(await summary.isVisible());
      await shot("review");
      await page.getByRole("button", { name: /^Codex ·/ }).click();
      await sheet.getByRole("button", { name: "Launch defaults in Settings" }).click();
      const claude = page.getByRole("checkbox", { name: "Claude Code: skip permissions by default" });
      const codex = page.getByRole("checkbox", { name: "Codex: skip permissions by default" });
      assert(await claude.isChecked()); assert(await codex.isChecked());
      await claude.uncheck();
      assert(await codex.isChecked());
      await codex.uncheck();
      await codex.scrollIntoViewIfNeeded();
      await shot("settings");
      await page.reload({ waitUntil: "networkidle" });
      const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("prefs")));
      assert.equal(stored.claudeBypassPermissions, false);
      assert.equal(stored.codexBypassPermissions, false);
      assert.deepEqual(errors, []);
      results.push({ ...sample, passed: true });
    } catch (error) {
      await page.screenshot({ path: join(out, `${sample.name}-failed.png`) });
      writeFileSync(join(out, `${sample.name}-failed.txt`), await page.locator("body").innerText());
      throw error;
    } finally { await context.close(); }
  }
} catch (error) { results.push({ passed: false, error: error.stack }); throw error; }
finally { await browser.close(); writeFileSync(join(out, "results.json"), JSON.stringify(results, null, 2) + "\n"); }
console.log(`Passed ${results.length} launch flags browser scenarios`);
