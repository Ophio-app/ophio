// SPDX-License-Identifier: Apache-2.0
// Run against the headless simulated client. No real desktop or microphone is used.
import assert from "node:assert/strict";
import { chromium } from "playwright";

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (e) => {
  if (e.type() === "error") errors.push(e.text());
});
const button = (name) => page.getByRole("button", { name, exact: true });
try {
  await page.goto(process.argv[2] ?? "http://127.0.0.1:5804", {
    waitUntil: "networkidle",
  });
  const id = await page.evaluate(async () => {
    localStorage.clear();
    const { seedAnnotation } = await import("/src/dev/sim/annotate.ts");
    const { UiStore } = await import("/src/app/ui.ts");
    const id = await seedAnnotation(window.__sim.host, "empty");
    const ui = new UiStore("sim-studio");
    ui.selectProject(window.__sim.host.projects[0].project_id);
    ui.push({ screen: "annotate", annotationId: id });
    return id;
  });
  await page.waitForTimeout(450);
  await page.reload({ waitUntil: "networkidle" });
  await page.waitForTimeout(900);
  await page.evaluate(() => {
    window.__u4Input = 0;
    window.__u4Mic = 0;
    const host = window.__sim.host,
      original = host.handleInput.bind(host);
    host.handleInput = (...args) => {
      window.__u4Input++;
      return original(...args);
    };
    navigator.mediaDevices.getUserMedia = async () => {
      window.__u4Mic++;
      throw new Error("Unexpected microphone request");
    };
  });
  const draft = () =>
    page.evaluate(
      (id) => JSON.parse(localStorage.getItem("annotations.sim-studio"))[id],
      id,
    );
  await button("Add source").click();
  await button("Capture the computer").click();
  await page.locator("#an-capture-targets").getByRole("button", { name: /DP-1/ }).click();
  await page.locator(".an-canvas image").waitFor();
  await button("Box").click();
  for (let n = 0; n < 3; n++) {
    await page.locator(".an-canvas").scrollIntoViewIfNeeded();
    const r = await page.locator(".an-canvas image").boundingBox();
    assert.ok(r);
    await page.mouse.move(
      r.x + r.width * (0.1 + n * 0.22),
      r.y + r.height * 0.25,
    );
    await page.mouse.down();
    await page.mouse.move(
      r.x + r.width * (0.24 + n * 0.22),
      r.y + r.height * 0.55,
      { steps: 8 },
    );
    await page.mouse.up();
  }
  const original = await draft();
  assert.deepEqual(
    original.marks.map((m) => m.label),
    ["A", "B", "C"],
  );
  assert.ok(original.sources[0].artifactId);
  assert.equal(original.sources[0].sha256.length, 64);
  // The view keys are for keyboards: hidden until focused.
  await button("Zoom in").focus();
  await page.keyboard.press("Enter");
  await button("Pan right").focus();
  await page.keyboard.press("Enter");
  await page.setViewportSize({ width: 800, height: 360 });
  await page.getByRole("button", { name: /^Fit picture/ }).click();
  const rotated = await draft();
  assert.deepEqual(rotated.sources, original.sources);
  assert.deepEqual(rotated.marks, original.marks);
  await page.setViewportSize({ width: 390, height: 844 });
  await page
    .getByLabel("What should change?", { exact: true })
    .fill("Give these more breathing room");
  // C, the mark just drawn, is already what the comment is about.
  await button("Target A").click();
  assert.equal(
    await page.evaluate(() => document.activeElement?.tagName),
    "TEXTAREA",
  );
  await button("Add comment").click();
  await page
    .getByLabel("What should change?", { exact: true })
    .fill("Increase contrast");
  // The targets carry over to the next comment until changed.
  await button("Target A").click();
  await button("Target C").click();
  await button("Target B").click();
  await button("Add comment").click();
  await button("Done typing").click();
  const linked = await draft();
  assert.deepEqual(
    linked.comments.map((c) => [...c.mark_ids].sort()),
    [
      [original.marks[0].mark_id, original.marks[2].mark_id].sort(),
      [original.marks[1].mark_id],
    ],
  );
  // The mark list is in the Draft sheet.
  await page.locator(".an-draft").click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Delete B", exact: true })
    .click();
  await button("Delete mark B").click();
  await page.locator(".an-draft").click();
  assert.equal(
    await page.getByText("Unresolved · B", { exact: true }).count(),
    1,
  );
  await page.keyboard.press("Escape");
  await button("Undo").click();
  assert.deepEqual((await draft()).marks, original.marks);
  assert.deepEqual((await draft()).comments, linked.comments);
  assert.deepEqual(
    await page.evaluate(() => [window.__u4Input, window.__u4Mic]),
    [0, 0],
  );
  console.log(
    "ok drawing, rotation, local pan/zoom, typed many-to-many links, no remote input or microphone, delete/undo",
  );

  await page
    .getByLabel("What should change?", { exact: true })
    .fill("Unsent text survives");
  await page.evaluate(() => window.__sim.offline());
  await page.reload({ waitUntil: "networkidle" });
  await page.waitForTimeout(700);
  assert.equal((await draft()).composer.text, "Unsent text survives");
  assert.deepEqual(
    (await draft()).sources,
    JSON.parse(JSON.stringify(original.sources)),
  );
  assert.deepEqual((await draft()).comments, linked.comments);
  console.log("ok IndexedDB source and draft recovery after reload");

  const derived = await page.evaluate(async (id) => {
    const { annotationStore, blobs, toBundle } =
      await import("/src/state/annotations.ts");
    const { deriveStill } = await import("/src/screens/annotate/sources.ts");
    const store = annotationStore("sim-studio"),
      source = store.get()[id].sources[0];
    store.update(id, (d) => ({
      ...d,
      marks: [],
      comments: [],
      composer: undefined,
    }));
    const redacted = await deriveStill(store, id, source, {
      kind: "redact",
      regions: [{ x: 0.2, y: 0.2, w: 0.2, h: 0.2 }],
    });
    const bitmap = await createImageBitmap(await blobs.get(redacted.blobKey));
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const ctx = canvas.getContext("2d");
    ctx.drawImage(bitmap, 0, 0);
    const pixel = [
      ...ctx.getImageData(
        Math.floor(bitmap.width * 0.3),
        Math.floor(bitmap.height * 0.3),
        1,
        1,
      ).data,
    ];
    bitmap.close();
    store.record(id, (d) => ({
      ...d,
      sources: d.sources.map((s) =>
        s.key === redacted.key
          ? { ...s, artifactId: "verified-test-artifact" }
          : s,
      ),
    }));
    const bundle = await toBundle({}, store, id);
    const cropped = await deriveStill(store, id, redacted, {
      kind: "crop",
      x: 0.1,
      y: 0.1,
      w: 0.5,
      h: 0.5,
    });
    return {
      pixel,
      redacted,
      cropped,
      keys: bundle.sources.map((s) => s.key),
      originalKey: source.key,
    };
  }, id);
  assert.deepEqual(derived.pixel, [0, 0, 0, 255]);
  assert.deepEqual(derived.keys, [derived.redacted.key]);
  assert.equal(derived.redacted.derivedFrom, derived.originalKey);
  assert.equal(derived.cropped.derivedFrom, derived.redacted.key);
  assert.equal(derived.cropped.width, Math.ceil(derived.redacted.width / 2));
  console.log(
    "ok decoded PNG redaction is opaque, outgoing bundle excludes original, crop keeps derivation",
  );

  await page.evaluate(async () => {
    const { UiStore } = await import("/src/app/ui.ts");
    new UiStore("sim-studio").push({ screen: "connections" });
  });
  await page.waitForTimeout(450);
  await page.reload({ waitUntil: "networkidle" });
  await page.waitForTimeout(900);
  await button("Link a chat").click();
  await page.getByLabel("Chat link code").fill("bad");
  await button("Check code").click();
  await page
    .locator("dialog[open]")
    .getByText(/No chat is waiting for this code/)
    .waitFor();
  await page.getByLabel("Chat link code").fill("123-456");
  await button("Check code").click();
  await button("Approve Robin Park").click();
  await page.getByRole("heading", { name: "Robin Park", exact: true }).first().waitFor();
  await button("Revoke Old tablet").click();
  await page
    .locator("dialog[open]")
    .getByRole("button", { name: "Revoke Old tablet", exact: true })
    .click();
  await page.waitForTimeout(300);
  assert.ok(
    await page.evaluate(
      () =>
        window.__sim.host.devices.find((d) => d.name === "Old tablet")
          .revoked_at,
    ),
  );
  await button("Revoke Pixel 9").click();
  await page
    .locator("dialog[open]")
    .getByRole("button", { name: "Revoke Pixel 9", exact: true })
    .click();
  await page
    .locator(".settings-screen")
    .getByText("This device’s access was revoked", { exact: true })
    .waitFor();
  console.log(
    "ok inline rejected chat code, accepted chat link, other phone and current phone revocation",
  );
  assert.deepEqual(errors, []);
} finally {
  await browser.close();
}
