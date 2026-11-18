// SPDX-License-Identifier: Apache-2.0
// Run against a local `vite --mode sim` server. No real computer or phone is used.
import { chromium } from "playwright";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";

const [base = "http://127.0.0.1:5918", out = join(tmpdir(), "ophio-checks-screenshots"), only] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ headless: true });
const scenes = ["project", "home", "empty", "check-details", "uncertain-job", "confirm-stopped", "draft-review", "receipt", "checking-task", "checking-chat"];
// Resolve the fixture IDs from the running simulator rather than duplicating them.
const probe = await browser.newPage();
await probe.goto(base);
await probe.waitForFunction(() => window.__sim?.host.projects.length);
const ids = await probe.evaluate(async () => {
  const { ids } = await import("/src/dev/sim/scenario.ts");
  const { checksIds } = await import("/src/dev/sim/checksFixtures.ts");
  return { tide: ids.pTide, notes: ids.pNotes, garden: ids.pGarden, headers: ids.tHeaders, checking: checksIds.checkingTask };
});
await probe.close();
const results = [];
try {
  for (const scene of scenes.filter((name) => !only || name === only)) for (const orientation of ["portrait", "landscape"]) for (const text of [1, 2]) for (const theme of ["light", "dark"]) {
    const viewport = orientation === "portrait" ? { width: 390, height: 844 } : { width: 844, height: 390 };
    const context = await browser.newContext({ viewport, colorScheme: theme, reducedMotion: "reduce", isMobile: true, hasTouch: true });
    const projectId = scene === "home" ? null : scene === "empty" ? ids.notes : scene.startsWith("checking-") ? ids.garden : ids.tide;
    const tab = scene === "checking-chat" ? "talk" : scene === "receipt" || scene === "checking-task" ? "work" : "library";
    const route = scene === "receipt" ? { screen: "receipt", taskId: ids.headers } : { screen: tab, ...(tab === "library" ? { part: "checks" } : {}) };
    const stack = tab === "library" ? [{ screen: "library" }] : tab === "talk" ? [{ screen: "talk" }] : [{ screen: "work" }, ...(scene === "receipt" ? [route] : [])];
    await context.addInitScript(({ projectId, tab, stack, theme, text, checking }) => {
      localStorage.clear();
      localStorage.setItem("prefs", JSON.stringify({ theme, reduceMotion: true, lastProfileId: "sim-studio", allComputers: false, allProjects: false }));
      localStorage.setItem("ui.sim-studio", JSON.stringify({ version: 2, state: { projectId, tab, stacks: { [tab]: stack }, ...(tab === "talk" ? { taskList: false, conversing: false, taskByProject: { [projectId]: checking } } : {}) } }));
      document.addEventListener("DOMContentLoaded", () => { document.documentElement.style.fontSize = `${16 * text}px`; });
    }, { projectId, tab, stack, theme, text, checking: ids.checking });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(String(e)));
    await page.goto(`${base}/?checks=1`);
    await page.waitForTimeout(1600);
    if (tab === "library") await page.getByRole("radio", { name: "Checks", exact: true }).click();
    await page.waitForTimeout(450);
    if (scene === "check-details") {
      await page.getByRole("button", { name: /^Unit tests/ }).click();
      await page.getByText("Details", { exact: true }).last().click();
      await page.locator(".checks-details").scrollIntoViewIfNeeded();
      if (text === 2 || orientation === "landscape") await page.getByText("Time limit", { exact: true }).evaluate((el) => el.scrollIntoView({ block: "center" }));
    }
    if (scene === "uncertain-job" || scene === "confirm-stopped") {
      await page.evaluate(async ({ projectId, checkId }) => {
        const host = window.__sim.host;
        host.projects.find(project => project.project_id === projectId).active_writer = null;
        host.checks.uncertainStop = true;
        // Start and stop before opening the sheet, so the fixture cannot finish
        // while a slow browser is finding the Stop button. The simulator holds
        // a new job as pending for a moment, and stopping a pending job simply
        // finishes it, so wait until the check is running before stopping it,
        // and until the stop is uncertain before showing it.
        const result = await host.inside({ command: "checks_run_now", project_id: projectId, check_ids: [checkId] });
        const reach = async (state) => {
          const deadline = Date.now() + 10_000;
          for (let job = host.checks.jobs.get(result.job.id); job.state !== state; job = host.checks.jobs.get(result.job.id)) {
            if (Date.now() > deadline) throw new Error(`The check job is ${job.state}; it never became ${state}`);
            await new Promise((resolve) => setTimeout(resolve, 20));
          }
        };
        await reach("running");
        await host.inside({ command: "checks_stop_job", job_id: result.job.id });
        await reach("uncertain");
        host.checks.uncertainFixture = result.job.id;
      }, { projectId: ids.tide, checkId: await page.evaluate(async () => (await import("/src/dev/sim/checksFixtures.ts")).checksIds.unit) });
      // The real screen gets its job from Run now; return the prepared uncertain
      // job on that request rather than writing React's private state.
      await page.evaluate(() => {
        const host = window.__sim.host, original = host.checks.act.bind(host.checks);
        host.checks.act = (command, grants) => command.command === "checks_run_now"
          ? Promise.resolve({ kind: "check_job", job: structuredClone(host.checks.jobs.get(host.checks.uncertainFixture)) })
          : original(command, grants);
      });
      await page.getByRole("button", { name: /^Unit tests/ }).click();
      await page.getByRole("button", { name: "Run it now", exact: true }).click();
      const action = page.getByRole("button", { name: "It has stopped", exact: true });
      await action.waitFor();
      await action.scrollIntoViewIfNeeded();
      if (scene === "uncertain-job") await action.evaluate((el) => el.closest(".notice").scrollIntoView({ block: "start" }));
      if (scene === "confirm-stopped") await action.click();
    }
    if (scene === "draft-review") await page.getByRole("button", { name: /Suggested checks/ }).first().click();
    if (scene === "receipt") {
      await page.getByText(/Ophio ran 4 checks/).waitFor();
      await page.getByRole("heading", { name: "Checks", exact: true }).evaluate((el) => el.scrollIntoView({ block: "start" }));
      if (orientation === "landscape") await page.locator(".receipt-checks").evaluate((el) => el.scrollIntoView({ block: "start" }));
    }
    if (scene === "checking-task") {
      await page.getByText(/Checking…/).first().waitFor();
      await page.getByText("Fix the garden app’s login screen", { exact: true }).scrollIntoViewIfNeeded();
    }
    if (scene === "checking-chat") {
      await page.locator(".chat-head__state").filter({ hasText: "Checking… · 2 of 4" }).waitFor();
      await page.locator(".chat-head").getByRole("button", { name: /^Stop/ }).waitFor();
      assert.equal(await page.locator(".talk-outcome").count(), 0, "A checking task has no finished outcome");
      assert.equal(await page.locator(".chat-head").evaluate((head) => {
        const heading = head.querySelector(".chat-head__heading");
        const state = head.querySelector(".chat-head__state");
        const bounds = heading.getBoundingClientRect(), status = state.getBoundingClientRect();
        return status.left >= bounds.left && status.right <= bounds.right + 1
          && state.scrollWidth <= state.clientWidth + 1 && bounds.bottom <= head.getBoundingClientRect().bottom + 1;
      }), true, "The checking count fits inside the header at large text");
    }
    await page.waitForTimeout(1000);
    const file = `${scene}-${orientation}-text${text * 100}-${theme}.png`;
    await page.screenshot({ path: join(out, file) });
    const overflow = await page.evaluate(() => {
      const root = document.documentElement;
      return { page: root.scrollWidth > root.clientWidth + 1, wide: [...document.querySelectorAll(".checks-body, .checks-sheet[open] .sheet__main, .checks-sheet[open] .sheet__body")].filter((el) => el.scrollWidth > el.clientWidth + 1).map((el) => el.className) };
    });
    assert.deepEqual(errors, [], `${file}: browser errors`);
    assert.equal(overflow.page, false, `${file}: page overflow`);
    assert.deepEqual(overflow.wide, [], `${file}: checks overflow`);
    results.push({ file, ...overflow });
    console.log(file);
    await context.close();
  }
  writeFileSync(join(out, "screenshots.json"), JSON.stringify(results, null, 2));
} finally { await browser.close(); }
