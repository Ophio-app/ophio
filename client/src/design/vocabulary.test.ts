// SPDX-License-Identifier: Apache-2.0
//
// Machine words kept reaching copy: "The computer accepted your decision.", a
// control that lasts "until its lease expires", a device's "grants". Every
// text a person can see is read from the source as the parser sees it, and
// none may use a word vocabulary.ts bans.
import { expect, it, vi } from "vitest";
import { BANNED } from "./vocabulary";

type Fs = {
  readFileSync(path: string, encoding: "utf8"): string;
  readdirSync(path: string, options: { recursive: true }): string[];
};
type Node = { type: string; start: number; [key: string]: unknown };

/** Attributes that name things for the code, not the person. */
const CODE_ATTRS = /^(className|key|id|htmlFor|role|type|name|href|src|data-[\w-]+)$/;
/** Places a string is a module, a type or a name rather than text. */
const CODE_PARENTS = /^(Import|Export\w*)Declaration$|^ImportExpression$|^TSLiteralType$|^TSExternalModuleReference$/;

/** The texts in a file that can reach the screen: JSX text, and strings that
 * read as words (with a space, or capitalised) rather than an identifier
 * such as "accepted" or "./lease". */
async function texts(file: string, source: string) {
  const { parseSync } = await vi.importActual<typeof import("vite")>("vite");
  const { program, errors } = parseSync(file, source);
  expect(errors.map((e) => `${file}: ${e.message}`)).toEqual([]);
  const found: { line: number; text: string }[] = [];
  const walk = (node: unknown, parent: Node | null) => {
    if (!node || typeof node !== "object") return;
    if (Array.isArray(node)) return node.forEach((n) => walk(n, parent));
    const n = node as Node;
    const text = n.type === "JSXText" ? (n.value as string)
      : n.type === "Literal" && typeof n.value === "string" ? n.value
      : n.type === "TemplateElement" ? ((n.value as { cooked: string | null; raw: string }).cooked ?? (n.value as { raw: string }).raw)
      : null;
    const code = parent && (CODE_PARENTS.test(parent.type)
      || (parent.type === "JSXAttribute" && CODE_ATTRS.test(String((parent.name as { name?: unknown })?.name))));
    if (text?.trim() && !code && (n.type === "JSXText" || /\s|^[A-Z]/.test(text.trim()))) {
      found.push({ line: source.slice(0, n.start).split("\n").length, text: text.trim().replace(/\s+/g, " ") });
    }
    for (const key in n) if (typeof n[key] === "object") walk(n[key], n);
  };
  walk(program, null);
  return found;
}

function banned(file: string, list: { line: number; text: string }[]) {
  return list.flatMap(({ line, text }) => BANNED.filter((b) => b.word.test(text))
    .map((b) => `${file}:${line}: “${text.match(b.word)![0]}” in “${text}”; say ${b.instead}`));
}

it("finds a banned word in what a person reads, and not in the code around it", async () => {
  const sample = `
    import { lease } from "./lease";
    type Phase = "accepted" | "sent";
    export function Card({ phase }: { phase: Phase }) {
      if (phase === "accepted") toast(\`Its lease expires at \${lease.at}\`);
      return <p className="host-note" data-host="x">The computer accepted your decision.</p>;
    }
    const LABEL = { accepted: "Accepted" };`;
  expect(banned("sample.tsx", await texts("sample.tsx", sample)).map((b) => b.split(":")[1])).toEqual(["5", "6", "8"]);
});

it("says what happened to the work, in the person's words", async () => {
  const fs = await vi.importActual<Fs>("node:fs");
  const found: string[] = [];
  for (const file of fs.readdirSync("src", { recursive: true })) {
    // The simulator's made-up content and the generated protocol types are
    // not copy; tests quote the old words to check they're gone, and the
    // glossary names the words it bans.
    if (!/\.tsx?$/.test(file) || /\.(test|fixtures)\.|\.d\.ts$|^dev\/|^protocol\/generated\/|^design\/vocabulary\.ts$/.test(file)) continue;
    found.push(...banned(file, await texts(file, fs.readFileSync(`src/${file}`, "utf8"))));
  }
  expect(found).toEqual([]);
  // It parses every source file, which takes several seconds on a busy CI host.
}, 60_000);
