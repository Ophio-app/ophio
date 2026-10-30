// SPDX-License-Identifier: Apache-2.0
// Optional local lint runner. Pass installed tooling paths; never installs packages.
// node src/screens/library/lint.mjs <eslint> <typescript-estree> <visitor-keys>
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url);
const [eslintPath, estreePath, keysPath] = process.argv.slice(2);
if (!eslintPath || !estreePath || !keysPath) {
  throw new Error(
    "Pass paths to installed ESLint, typescript-estree, and visitor-keys modules.",
  );
}
const { ESLint } = require(eslintPath);
const { parse } = require(estreePath);
const { visitorKeys } = require(keysPath);
const lint = new ESLint({
  cwd: fileURLToPath(new URL("../../../", import.meta.url)),
  overrideConfigFile: true,
  overrideConfig: [
    {
      files: ["**/*.ts", "**/*.tsx"],
      languageOptions: {
        parser: {
          parseForESLint(code) {
            return {
              ast: parse(code, {
                loc: true,
                range: true,
                comment: true,
                tokens: true,
                jsx: true,
              }),
              visitorKeys,
            };
          },
        },
      },
      rules: {
        "no-debugger": "error",
        "no-unreachable": "error",
        "no-duplicate-case": "error",
        "no-async-promise-executor": "error",
        "no-constant-condition": "error",
        eqeqeq: ["error", "always", { null: "ignore" }],
      },
    },
  ],
});
const results = await lint.lintFiles([
  "src/screens/library",
  "src/screens/transfers",
  "src/dev/sim/library.ts",
  "src/dev/sim/transfers.ts",
]);
console.log(await (await lint.loadFormatter("stylish")).format(results));
console.log(
  `Lint: ${results.length} files, ${results.reduce((n, r) => n + r.errorCount, 0)} errors, ${results.reduce((n, r) => n + r.warningCount, 0)} warnings`,
);
process.exitCode = results.some((r) => r.errorCount || r.warningCount) ? 1 : 0;
