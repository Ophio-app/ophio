// SPDX-License-Identifier: Apache-2.0
//
// Settings once had two icons: the cog in the header and a slider glyph on
// GitHub and the project's rows. Pause had two, checkboxes came in four sizes
// and GitHub turned settings on with a switch where every other screen used a
// tick. One action has one look; these keep the second ones from coming back.
import { expect, it, vi } from "vitest";

type Fs = {
  readFileSync(path: string, encoding: "utf8"): string;
  readdirSync(path: string, options: { recursive: true }): string[];
};
const sources = async (ext: string) => {
  // Vitest empties CSS imports, so the files are read from disk.
  const fs = await vi.importActual<Fs>("node:fs");
  return fs.readdirSync("src", { recursive: true })
    .filter((f) => f.endsWith(ext) && !f.includes(".test."))
    .map((f) => ({ file: f, text: fs.readFileSync(`src/${f}`, "utf8") }));
};

/** The icons lucide offers twice over, with the one Ophio uses for each. */
const SECOND_ICONS: Record<string, string> = {
  Settings2: "Settings", Cog: "Settings", SettingsIcon: "Settings",
  PauseCircle: "Pause", CirclePause: "Pause", PlayCircle: "Play", CirclePlay: "Play",
  SquarePen: "Pencil", PenLine: "Pencil", Edit: "Pencil", Edit2: "Pencil", Edit3: "Pencil",
};

it("draws each action with one icon", async () => {
  const found: string[] = [];
  for (const { file, text } of await sources(".tsx")) {
    for (const m of text.matchAll(/import\s*\{([^}]*)\}\s*from\s*"lucide-react"/g)) {
      for (const name of m[1]!.split(",").map((n) => n.trim().split(/\s+as\s+/)[0]!)) {
        if (name in SECOND_ICONS) found.push(`${file}: ${name}, not ${SECOND_ICONS[name]}`);
      }
    }
    // Anything named settings opens with the cog.
    for (const m of text.matchAll(/icon=\{(\w+)\}[^>]*?(?:label|title)="([^"]*\b[Ss]ettings)"/g)) {
      if (m[1] !== "Settings") found.push(`${file}: “${m[2]}” uses ${m[1]}`);
    }
  }
  expect(found).toEqual([]);
});

it("sizes and colours every checkbox and radio in one place", async () => {
  const design = (await sources(".css")).find((s) => s.file === "design/components.css")!;
  expect(design.text).toMatch(/:where\(input\[type="checkbox"\], input\[type="radio"\]\) \{[^}]*width: 1\.25rem;[^}]*accent-color: var\(--accent\);/);
  const restyled: string[] = [];
  for (const { file, text } of await sources(".css")) {
    const css = text.replace(/\/\*[\s\S]*?\*\//g, "");
    for (const m of css.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
      const selector = m[1]!.trim(), body = m[2]!;
      if (!/\binput\b/.test(selector) || /range|progress|:where/.test(selector)) continue;
      if (/accent-color|appearance:\s*none/.test(body)) restyled.push(`${file}: ${selector}`);
    }
  }
  expect(restyled).toEqual([]);
});

it("draws every error in the app's lime, not the red of Stop", async () => {
  const red: string[] = [];
  for (const { file, text } of await sources(".css")) {
    const css = text.replace(/\/\*[\s\S]*?\*\//g, "");
    for (const m of css.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
      if (/error|alert|notice--stop|toast--stop/.test(m[1]!) && /--stop/.test(m[2]!)) red.push(`${file}: ${m[1]!.trim()}`);
    }
  }
  expect(red).toEqual([]);
});
