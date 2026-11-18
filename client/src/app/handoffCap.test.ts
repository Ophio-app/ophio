// SPDX-License-Identifier: Apache-2.0
import { expect, it, vi } from "vitest";

// jsdom has no layout engine. This guards the connected portal's CSS contract;
// scripts/handoff-cap.mjs checks actual geometry, scrolling and Stop hit targets.
async function styles() {
  const fs = await vi.importActual<{ readFileSync(path: string, encoding: "utf8"): string }>("node:fs");
  const css = fs.readFileSync("src/app/split.css", "utf8");
  return { css, stack: css.match(/\.split-global\s*\{([^}]+)\}/)?.[1] ?? "" };
}

it("caps all retained handoff warnings together relative to the connected frame", async () => {
  const { css, stack } = await styles();
  expect(stack).toMatch(/max-height:\s*35%\s*;/);
  expect(stack).toMatch(/flex:\s*none\s*;/);
  expect(css).toMatch(/\.split-space\s*\{[^}]*flex:\s*1\s*;[^}]*min-height:\s*0\s*;/);
});

it("keeps every retained warning reachable by scrolling inside the capped stack", async () => {
  const { stack } = await styles();
  expect(stack).toMatch(/overflow-y:\s*auto\s*;/);
  expect(stack).toMatch(/overscroll-behavior-y:\s*contain\s*;/);
});
