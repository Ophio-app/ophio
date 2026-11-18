// SPDX-License-Identifier: Apache-2.0
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { checkRequestGates } from "./request-gate-check.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const files = [
  "crates/services/src/runtime.rs",
  ...new Set([...readFileSync(resolve(root, "crates/services/src/runtime.rs"), "utf8").matchAll(/protocol::(\w+)::\w+\.into\(\)/g)].map((match) => `crates/protocol/src/${match[1]}.rs`)),
  "crates/protocol/src/command.rs", "crates/protocol/src/lib.rs", "crates/protocol/src/github.rs",
  "client/src/protocol/generated/Command.ts", "client/src/protocol/generated/Query.ts", "client/src/protocol/generated/GithubAction.ts",
  "client/src/host/requestPolicy.ts", "client/src/dev/sim/githubGrants.ts",
];
async function mutation(change, verify) {
  const temp = mkdtempSync(resolve(tmpdir(), "ophio-request-gates-"));
  try {
    for (const file of files) {
      const path = resolve(temp, file);
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, readFileSync(resolve(root, file)));
    }
    const edit = (file, transform) => {
      const path = resolve(temp, file);
      writeFileSync(path, transform(readFileSync(path, "utf8")));
    };
    change(edit);
    verify(await checkRequestGates(temp));
  } finally { rmSync(temp, { recursive: true, force: true }); }
}

test("the current Rust authority, generated types and enforcement policies agree", async () => {
  assert.deepEqual(await checkRequestGates(root), []);
});

for (const [name, tag, rust] of [["Command", "command", "command.rs"], ["Query", "kind", "lib.rs"], ["GithubAction", "action", "github.rs"]]) {
  test(`an inserted Rust ${name} fails even before regenerating types`, async () => {
    await mutation((edit) => edit(`crates/protocol/src/${rust}`, (text) => text.replace(`pub enum ${name} {`, `pub enum ${name} {\n    InsertedMissingRequest,`)), (errors) => {
      assert(errors.some((error) => error.includes(`${name}.inserted_missing_request`) && error.includes("regenerate")), errors.join("\n"));
      assert(errors.some((error) => error.includes(`${name}.inserted_missing_request`) && error.includes("explicit entry")), errors.join("\n"));
      assert(errors.some((error) => error.includes(name === "GithubAction" ? 'required grants, such as ["tasks"]' : 'use { feature: "advertised_feature_name" }')), errors.join("\n"));
    });
  });
  test(`an inserted generated ${name} fails with an actionable missing-policy message`, async () => {
    await mutation((edit) => edit(`client/src/protocol/generated/${name}.ts`, (text) => text.replace(`export type ${name} = `, `export type ${name} = { "${tag}": "inserted_missing_request" } | `)), (errors) => {
      assert(errors.some((error) => error.includes(`${name}.inserted_missing_request`) && error.includes("explicit entry") && error.includes("client/src/")), errors.join("\n"));
    });
  });
}

test("an unreasoned allow exception fails", async () => {
  await mutation((edit) => edit("client/src/host/requestPolicy.ts", (text) => text.replace("snapshot: { allow: baseline }", 'snapshot: { allow: "" }')), (errors) => {
    assert(errors.some((error) => error.includes("Query.snapshot") && error.includes("reasoned allow")), errors.join("\n"));
  });
});

test("removing a simulator grant entry fails instead of falling back to admin", async () => {
  await mutation((edit) => edit("client/src/dev/sim/githubGrants.ts", (text) => text.replace('  merge_pr: ["tasks", "approvals"],\n', "")), (errors) => {
    assert(errors.some((error) => error.includes("GithubAction.merge_pr") && error.includes("githubGrants.ts")), errors.join("\n"));
  });
});


test("an unrelated runtime string does not count as an advertised feature", async () => {
  await mutation((edit) => {
    edit("client/src/host/requestPolicy.ts", (text) => text.replace('feature: "reports_v1"', 'feature: "unrelated_v1"'));
    edit("crates/services/src/runtime.rs", (text) => text + '\nfn unrelated() -> String { "unrelated_v1".into() }\n');
  }, (errors) => {
    assert(errors.some((error) => error.includes("Command.submit_report") && error.includes("not advertised")), errors.join("\n"));
  });
});

test("a feature that is declared but not advertised fails", async () => {
  await mutation((edit) => edit("client/src/host/requestPolicy.ts", (text) => text.replace('feature: "reports_v1"', 'feature: "invented_unadvertised_v1"')), (errors) => {
    assert(errors.some((error) => error.includes("Command.submit_report") && error.includes("not advertised")), errors.join("\n"));
  });
});
