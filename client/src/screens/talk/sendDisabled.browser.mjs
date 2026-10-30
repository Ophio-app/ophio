// SPDX-License-Identifier: Apache-2.0
// A disabled primary action must not look ready in either theme. Fading the
// accent left it bright on the dark page, so a disabled Send read as enabled. jsdom applies no stylesheets; this reads the rendered colours.
// node src/screens/talk/sendDisabled.browser.mjs http://127.0.0.1:5959
import { chromium } from "playwright";
import assert from "node:assert/strict";

const [url] = process.argv.slice(2);
assert(url, "Provide a simulated app URL");
const browser = await chromium.launch({ headless: true });
try {
  for (const colorScheme of ["light", "dark"]) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, colorScheme, reducedMotion: "reduce" });
    const page = await context.newPage(), errors = [];
    page.on("pageerror", e => errors.push(String(e)));
    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(1000);
    assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), colorScheme);
    const send = page.locator(".composer__send").first();
    const pic = page.locator(".composer__pic").first();
    await send.waitFor({ state: "attached" });
    assert(await send.isDisabled(), "An empty draft has nothing to send");
    // As in a chat app, an empty field ends in the screenshot, and Send takes its place once there are words.
    assert(await pic.isVisible() && !(await send.isVisible()), `${colorScheme}: an empty field shows the screenshot, not Send`);
    await page.locator(".composer__field textarea").first().fill("Check the p95 line");
    await page.waitForTimeout(400);
    assert(await send.isVisible() && await send.isEnabled() && !(await pic.isVisible()), `${colorScheme}: words in the field bring Send in place of the screenshot`);
    await page.locator(".composer__field textarea").first().fill("");
    const looks = await page.evaluate(() => {
      const probe = (html) => {
        const holder = document.createElement("div");
        holder.innerHTML = html;
        document.querySelector(".composer")?.append(holder);
        const [on, off] = holder.children;
        const look = (el) => { const s = getComputedStyle(el); return { background: s.backgroundColor, color: s.color, opacity: s.opacity }; };
        const out = { on: look(on), off: look(off) };
        holder.remove();
        return out;
      };
      return {
        send: probe('<button class="iconbtn iconbtn--primary composer__send">+</button><button class="iconbtn iconbtn--primary composer__send" disabled>+</button>'),
        icon: probe('<button class="iconbtn iconbtn--primary">+</button><button class="iconbtn iconbtn--primary" disabled>+</button>'),
        text: probe('<button class="btn btn--primary">Go</button><button class="btn btn--primary" disabled>Go</button>'),
      };
    });
    // Send is the filled circle at the end of the field, so it is held to the same.
    for (const [name, { on, off }] of Object.entries(looks)) {
      assert.notEqual(off.background, on.background, `${colorScheme}: a disabled ${name} primary keeps the ready fill`);
      assert.equal(off.opacity, "1", `${colorScheme}: a disabled ${name} primary is shown by its own colours, not by fading`);
    }
    assert.deepEqual(errors, []);
    await context.close();
  }
  console.log("disabled primary actions look disabled in light and dark");
} finally {
  await browser.close();
}
