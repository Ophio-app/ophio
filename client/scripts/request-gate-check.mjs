// SPDX-License-Identifier: Apache-2.0
// Run from any directory: node client/scripts/request-gate-check.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve, dirname } from "node:path";
import { stripTypeScriptTypes } from "node:module";

const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const inventories = [
  ["Command", "command", "crates/protocol/src/command.rs", "client/src/host/requestPolicy.ts", "commandPolicy"],
  ["Query", "kind", "crates/protocol/src/lib.rs", "client/src/host/requestPolicy.ts", "queryPolicy"],
  ["GithubAction", "action", "crates/protocol/src/github.rs", "client/src/dev/sim/githubGrants.ts", "githubActionGrants"],
];

function wireName(name) {
  return name.replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2").replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
}

function generatedVariants(source, name, tag) {
  const declaration = source.replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, "").split(`export type ${name} = `)[1];
  if (!declaration) throw new Error(`Cannot read generated ${name}; update the inventory parser.`);
  let depth = 0;
  const variants = [];
  for (const token of declaration.matchAll(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|[{};]/g)) {
    if (token[0] === ";" && depth === 0) break;
    if (token[0] === "{") {
      if (depth === 0) {
        const variant = declaration.slice(token.index).match(new RegExp(`^\\{\\s*"${tag}"\\s*:\\s*"([^"]+)"`));
        if (!variant) throw new Error(`Cannot read ${name}.${tag}; update the inventory parser.`);
        variants.push(variant[1]);
      }
      depth++;
    } else if (token[0] === "}") depth--;
  }
  if (!variants.length || depth !== 0) throw new Error(`Cannot read generated ${name} union; update the inventory parser.`);
  return variants;
}

async function loadSource(source) {
  const js = stripTypeScriptTypes(source);
  return import(`data:text/javascript;base64,${Buffer.from(js).toString("base64")}`);
}

/** Reads the Rust authority as well as generated types, so stale generation
 * cannot hide a newly added request. Unsupported enum shapes fail loudly. */
export async function checkRequestGates(root = defaultRoot) {
  const errors = [];
  // Match the actual capability builder, including protocol feature constants.
  // A declaration elsewhere is not evidence that the computer advertises it.
  const runtime = readFileSync(resolve(root, "crates/services/src/runtime.rs"), "utf8").replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, "");
  const features = runtime.match(/features:\s*vec!\[([\s\S]*?)\.collect\(\)/)?.[1];
  if (!features) throw new Error("Cannot read the advertised features in crates/services/src/runtime.rs; update the capability-builder parser.");
  const advertised = new Set([...features.matchAll(/"([^"\n]+)"\.into\(\)/g)].map((match) => match[1]));
  for (const match of features.matchAll(/protocol::(\w+)::(\w+)\.into\(\)/g)) {
    const constants = readFileSync(resolve(root, `crates/protocol/src/${match[1]}.rs`), "utf8");
    const value = constants.match(new RegExp(`const ${match[2]}: &str = "([^"]+)"`))?.[1];
    if (value) advertised.add(value);
  }
  for (const [name, tag, rustPath, policyPath, exportName] of inventories) {
    const rust = readFileSync(resolve(root, rustPath), "utf8");
    const body = rust.match(new RegExp(`pub enum ${name} \\{\\n([\\s\\S]*?)^\\}`, "m"))?.[1];
    if (!body) throw new Error(`Cannot read ${rustPath}: ${name}; update the inventory parser.`);
    const authority = [...body.matchAll(/^    ([A-Z][A-Za-z0-9_]*)(?=\s*[{,(])/gm)].map((m) => wireName(m[1]));
    if (!authority.length) throw new Error(`No ${name} variants found in ${rustPath}.`);
    const generated = generatedVariants(readFileSync(resolve(root, `client/src/protocol/generated/${name}.ts`), "utf8"), name, tag);
    for (const item of authority) if (!generated.includes(item)) errors.push(`${name}.${item}: regenerate the phone protocol types; the Rust variant is missing there.`);
    for (const item of generated) if (!authority.includes(item)) errors.push(`${name}.${item}: generated types do not match Rust; regenerate the phone protocol types.`);
    const policy = (await loadSource(readFileSync(resolve(root, policyPath), "utf8")))[exportName];
    if (!policy || typeof policy !== "object") throw new Error(`Cannot read ${exportName} in ${policyPath}.`);
    for (const item of new Set([...authority, ...generated])) {
      const entry = policy[item];
      if (!Object.hasOwn(policy, item)) {
        const decision = name === "GithubAction"
          ? 'choose a nonempty list of required grants, such as ["tasks"], matching the computer permission rules'
          : 'use { feature: "advertised_feature_name" } (checks_v1 for Checks requests), or { allow: "One-line compatibility reason." }';
        errors.push(`${name}.${item}: add an explicit entry to ${policyPath} (${exportName}); ${decision}.`);
      } else if (name === "GithubAction") {
        if (!Array.isArray(entry) || !entry.length) errors.push(`${name}.${item}: choose at least one required grant in ${policyPath}.`);
      } else if (!entry || typeof entry !== "object" || (Object.hasOwn(entry, "feature") === Object.hasOwn(entry, "allow")) || !(typeof entry.feature === "string" && entry.feature.trim() || typeof entry.allow === "string" && entry.allow.trim().length >= 20)) {
        errors.push(`${name}.${item}: provide a nonempty feature or a reasoned allow exception in ${policyPath}.`);
      }
    }
    if (name !== "GithubAction") {
      for (const [item, entry] of Object.entries(policy)) {
        if (entry?.feature && !advertised.has(entry.feature)) errors.push(`${name}.${item}: feature "${entry.feature}" is not advertised in crates/services/src/runtime.rs; advertise it there, or extend the checker for a new capability-builder shape.`);
      }
    }
    for (const item of Object.keys(policy)) if (!authority.includes(item)) errors.push(`${name}.${item}: remove the stale entry from ${policyPath}.`);
  }
  return errors;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const errors = await checkRequestGates();
    if (errors.length) {
      console.error(`Request compatibility check failed:\n${errors.map((e) => `- ${e}`).join("\n")}`);
      process.exitCode = 1;
    } else console.log("Request compatibility check passed: every Rust and phone request and GitHub action has an explicit policy.");
  } catch (error) {
    console.error(`Request compatibility check could not run: ${error.message}`);
    process.exitCode = 1;
  }
}
