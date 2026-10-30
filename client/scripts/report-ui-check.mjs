// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { chromium } from "playwright";

const base = process.argv[2] ?? "http://localhost:5807";
const output = process.argv[3];
if (output) await mkdir(output, { recursive: true });
const expectedGpu = process.env.OPHIO_CHECK_GPU;
const browser = await chromium.launch({ headless: true, args: expectedGpu ? ["--use-angle=vulkan", "--enable-features=Vulkan", "--ignore-gpu-blocklist"] : [] });
const page = await browser.newPage({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1 });
const errors = [];
page.on("pageerror", e => errors.push(String(e)));
const button = name => page.getByRole("button", { name, exact: true });
const sheet = () => page.getByRole("dialog", { name: "Report a problem", exact: true });
async function shot(name) { if (output) await page.screenshot({ animations: "disabled", path: `${output}/${name}.png` }); }
async function settings(reportId) {
  await page.evaluate(async id => {
    const { UiStore } = await import("/src/app/ui.ts");
    const ui = new UiStore("sim-studio");
    ui.push({ screen: "settings", ...(id === null ? {} : { section: "settings-reports", ...(id ? { reportId: id } : {}) }) });
    ui.persistNow();
  }, reportId);
  await page.reload({ waitUntil: "networkidle" });
  await page.getByRole("heading", { name: reportId === null ? "Settings" : "Your reports", exact: true }).waitFor();
}
try {
  await page.goto(`${base}/?report-phone`, { waitUntil: "networkidle" });
  const renderer = await page.evaluate(() => { const gl = document.createElement("canvas").getContext("webgl"); const ext = gl?.getExtension("WEBGL_debug_renderer_info"); return ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : "unavailable"; });
  if (expectedGpu) assert.match(renderer, new RegExp(expectedGpu), `Expected ${expectedGpu}, got ${renderer}`);
  await page.waitForFunction(() => window.__sim);
  await settings(null);
  await page.getByRole("button", { name: /^Report a problem/ }).click();
  await sheet().waitFor();
  assert.equal(await sheet().getByRole("img").count(), 0);
  await shot("sheet-without-picture");
  await page.getByLabel("What happened?").fill("The task stayed waiting after I tapped it.");
  await button("See all ›").click();
  await page.getByLabel("Real names for the pseudonyms").check();
  await page.getByText("Sample project", { exact: true }).waitFor();
  await page.getByLabel("Real names for the pseudonyms").scrollIntoViewIfNeeded();
  await shot("see-all");
  await button("Send").click();
  await page.getByText(/Got it, report/).waitFor();
  await shot("got-it");
  await button("Done").click();
  await page.waitForFunction(() => window.__sim.host.reports.view.reports.length === 1);
  const reportId = await page.evaluate(() => window.__sim.host.reports.view.reports[0].report_id);
  await page.getByRole("button", { name: /^Your reports/ }).click();
  await page.getByRole("heading", { name: "Your reports", exact: true }).waitFor();
  await shot("your-reports");
  await page.getByRole("button", { name: /^Report [A-Z0-9]{6}/ }).click();
  await page.getByText("Has it happened again since you reported it?", { exact: true }).waitFor();
  await page.evaluate(() => {
    window.__answerSeen = false;
    const observer = new MutationObserver(() => { if (document.body.textContent.includes("Answered: Yes")) { window.__answerSeen = true; observer.disconnect(); } });
    observer.observe(document.body, { childList: true, subtree: true, characterData: true });
  });
  await button("Yes").click();
  await page.waitForFunction(() => window.__answerSeen);
  await page.getByText("Fixed in this update", { exact: true }).waitFor();
  await button("Update now").waitFor();
  await shot("fixed-update");
  await button("Update now").click();
  await page.waitForFunction(() => window.__sim.device.installs.length === 1);

  await page.evaluate(() => { window.__sim.reports.capture = true; window.__sim.reports.open(); });
  await sheet().getByRole("img").waitFor();
  await shot("sheet-with-picture");
  await button("Mark up this screen").click();
  const markup = page.getByRole("dialog", { name: "Mark this screen", exact: true });
  await markup.locator(".an-canvas image").waitFor();
  await markup.getByRole("button", { name: "Box", exact: true }).click();
  const bounds = await markup.locator(".an-canvas image").boundingBox();
  await page.mouse.move(bounds.x + bounds.width * 0.15, bounds.y + bounds.height * 0.25);
  await page.mouse.down();
  await page.mouse.move(bounds.x + bounds.width * 0.8, bounds.y + bounds.height * 0.5, { steps: 8 });
  await page.mouse.up();
  await markup.getByRole("button", { name: "Keep marks", exact: true }).click();
  await markup.waitFor({ state: "hidden" });
  await button("Remove").click(); await sheet().getByRole("img").waitFor({ state: "detached" }); await shot("sheet-picture-removed");
  await button("Add the picture back").click(); await sheet().getByRole("img").waitFor();
  await button("Send").click(); await page.getByText(/Got it, report/).waitFor();
  await page.waitForFunction(() => window.__sim.host.reports.view.reports.length === 2);
  const picture = await page.evaluate(() => [...window.__sim.host.reports.details.values()].find(d => d.bundle.screenshot)?.bundle.screenshot);
  assert.equal(picture.marked, true); assert.ok(Buffer.from(picture.data, "base64").length <= 512 * 1024);
  await button("Done").click();
  await page.evaluate(() => window.__sim.host.reports.crash());
  await page.getByRole("dialog", { name: "Ophio closed unexpectedly", exact: true }).waitFor();
  await shot("crash-prompt"); await button("Not now").click();
  await page.evaluate(() => window.__sim.reports.screenshot());
  await button("Report").waitFor(); await shot("screenshot-chip");
  await button("Report").click(); await sheet().waitFor();
  await sheet().getByRole("button", { name: "Close", exact: true }).click();
  await page.evaluate(() => document.documentElement.style.fontSize = "200%");
  await page.evaluate(() => window.__sim.reports.open()); await sheet().waitFor();
  assert.ok(await button("Send").isVisible());
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await shot("sheet-large-text");
  assert.deepEqual(errors, []);
  console.log(`Report UI checks passed; renderer: ${renderer}; report ${reportId}`);
} finally { await browser.close(); }
