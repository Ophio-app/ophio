// SPDX-License-Identifier: Apache-2.0
// Supplemental headless check: real pointer events, Back and viewport rotation.
// node src/screens/library/completion.browser.mjs http://127.0.0.1:5911 /tmp/library-recovery-shots
import { chromium } from "playwright";
import { readFileSync, mkdirSync } from "node:fs";
import assert from "node:assert/strict";
import { join } from "node:path";

const [url, out] = process.argv.slice(2);
assert(url && out, "Provide the simulator URL and screenshot directory");
mkdirSync(out, { recursive: true });
const plan = JSON.parse(readFileSync("scripts/plans/library-recovery.json", "utf8"));
const route = plan.find((s) => s.name === "L1-preview-light").steps[0][1];
const browser = await chromium.launch({ headless: true });
try {
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    reducedMotion: "reduce",
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  await page.goto(url);
  await page.waitForTimeout(1500);
  await page.evaluate(route);
  await page.waitForTimeout(3000);
  await page.getByRole("button", { name: "Add preview", exact: true }).click();
  const label = page.getByLabel("Preview label"),
    address = page.getByLabel("Preview URL");
  await label.fill("Keep this review");
  await address.fill("http://localhost:3000/review");
  const intact = async () => {
    assert.equal(await label.inputValue(), "Keep this review");
    assert.equal(await address.inputValue(), "http://localhost:3000/review");
  };
  await page.setViewportSize({ width: 844, height: 390 });
  await intact();
  await page.screenshot({ path: join(out, "preview-rotation.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await address.focus();
  // Viewport reduction models keyboard occlusion; the pointer is a real click.
  await page.setViewportSize({ width: 390, height: 560 });
  const box = await page.getByRole("dialog").boundingBox();
  assert(box && box.y > 0, "A backdrop area must be available");
  await page.mouse.click(5, Math.max(2, box.y / 2));
  assert(
    await page.getByRole("dialog").isVisible(),
    "The first outside tap while typing must keep the sheet open",
  );
  await intact();
  await page.screenshot({
    path: join(out, "preview-keyboard-backdrop.png"),
  });
  await page.evaluate(() => history.back());
  await page.waitForTimeout(300);
  assert.equal(await page.getByRole("dialog").count(), 0);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("button", { name: "Back", exact: true }).click();
  await page.waitForTimeout(300);
  await page.evaluate(route);
  await page.waitForTimeout(3000);
  await page.getByRole("button", { name: "Add preview", exact: true }).click();
  await intact();
  await page.screenshot({
    path: join(out, "preview-navigation-restart.png"),
  });
  await page
    .getByRole("button", { name: "Cancel and clear draft", exact: true })
    .click();
  await page.getByRole("button", { name: "Add preview", exact: true }).click();
  assert.equal(await label.inputValue(), "");
  assert.equal(await address.inputValue(), "");
  assert.deepEqual(errors, []);
  console.log(
    "ok pointer backdrop, Back, navigation, rotation, restart and explicit clear",
  );
} finally {
  await browser.close();
}
