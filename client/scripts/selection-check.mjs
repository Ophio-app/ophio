// SPDX-License-Identifier: Apache-2.0
// Headless simulator only; no native agents, accounts or personal devices.
// node scripts/selection-check.mjs http://127.0.0.1:5967 <evidence-directory>
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
  ]) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, reducedMotion: "reduce", hasTouch: true });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", e => errors.push(e.message));
    try {
      await page.goto(base, { waitUntil: "networkidle" });
      await page.waitForFunction(() => !!window.__sim);
      const oldTitle = await page.evaluate(() => {
        const host = window.__sim.host;
        const task = host.tasks.find(t => t.state === "completed");
        if (!task) throw new Error("Expected a completed simulator task");
        task.harness = "claude_code"; task.model = "claude-opus-5-5"; task.effort = "high"; task.updated_at = Date.now();
        host.emit({ type: "task_upserted", task });
        const project = host.projects.find(p => p.project_id === task.project_id);
        project.active_writer = null;
        host.emit({ type: "project_upserted", project });
        const run = host.runs.find(r => r.task_id === task.task_id);
        if (run) {
          run.harness = task.harness; run.requested_model = task.model; run.actual_model = task.model;
          host.emit({ type: "run_upserted", run });
        }
        return task.title;
      });
      await page.locator(".chat-head__back").click();
      await page.getByRole("button", { name: /^New/ }).first().click();
      // A project that can use GitHub asks what's new first.
      const task = page.getByRole("button", { name: /^New task/ });
      if (await task.count()) await task.first().click();
      await page.locator(".composer__chip").filter({ hasText: "Claude Code" }).click();
      const sheet = page.getByRole("dialog", { name: "Agent for this task" });
      await sheet.getByRole("group", { name: "Agent", exact: true }).getByRole("button", { name: /Codex/ }).click();
      await sheet.getByRole("group", { name: "Model", exact: true }).getByRole("button", { name: /GPT-6-Astra/ }).click();
      await sheet.getByRole("group", { name: "Effort", exact: true }).getByRole("button", { name: "High", exact: true }).click();
      await sheet.getByRole("button", { name: "Use this agent", exact: true }).click();
      await page.getByRole("button", { name: new RegExp(`^${oldTitle}`) }).click();
      const input = page.locator(".composer textarea");
      await input.fill("Read the git history");
      await page.setViewportSize({ width: sample.width, height: sample.height });
      if (sample.text) await page.evaluate(text => { document.documentElement.style.fontSize = `${text * 100}%`; }, sample.text);
      await page.waitForTimeout(250);
      const continuation = page.getByRole("button", { name: "Continue", exact: true });
      assert(await continuation.isDisabled(), "An old Claude task needs an explicit recipient decision");
      await input.press("Control+Enter");
      await page.waitForTimeout(250);
      assert(await continuation.isDisabled(), "The keyboard cannot bypass the recipient decision");
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No horizontal overflow");
      await page.screenshot({ path: join(out, `${sample.name}-recipient.png`) });
      await input.scrollIntoViewIfNeeded();
      const fieldBox = await input.boundingBox();
      assert(fieldBox && fieldBox.y >= 0 && fieldBox.y + fieldBox.height <= sample.height, "The composer can be reached at keyboard height and large text");
      await input.click();
      await page.screenshot({ path: join(out, `${sample.name}-composer.png`) });
      const newTask = page.getByRole("button", { name: "New task with this selection", exact: true });
      await newTask.scrollIntoViewIfNeeded();
      const box = await newTask.boundingBox();
      assert(box && box.y >= 0 && box.y + box.height <= sample.height, "New-task recovery is reachable");
      await newTask.click();
      await page.getByRole("heading", { name: "Review new task", exact: true }).waitFor();
      assert(await page.getByRole("button", { name: /Codex · GPT-6-Astra · High/ }).count(), "Review keeps the selected agent and model");
      assert.equal(await page.getByRole("textbox", { name: "What you’re asking", exact: true }).inputValue(), "Read the git history");
      await page.screenshot({ path: join(out, `${sample.name}-review.png`) });
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
console.log(`Passed ${results.length} selection browser scenarios`);
