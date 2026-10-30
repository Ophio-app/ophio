// SPDX-License-Identifier: Apache-2.0
// Optional local lint runner: point ANNOTATE_LINT_MODULES at existing ESLint/parser modules.
// This uses already installed tooling without changing the shared dependencies.
import { createRequire } from "node:module";
import { resolve } from "node:path";
const require = createRequire(
  resolve(process.env.ANNOTATE_LINT_MODULES ?? "node_modules", "package.json"),
);
export default [
  {
    files: ["**/*.ts", "**/*.tsx"],
    languageOptions: {
      parser: require("@typescript-eslint/parser"),
      parserOptions: {
        ecmaVersion: "latest",
        sourceType: "module",
        ecmaFeatures: { jsx: true },
      },
    },
    plugins: { "react-hooks": require("eslint-plugin-react-hooks") },
    rules: {
      "constructor-super": "error",
      "no-constant-condition": "error",
      "no-dupe-args": "error",
      "no-dupe-else-if": "error",
      "no-dupe-keys": "error",
      "no-sparse-arrays": "error",
      "no-unreachable": "error",
      "no-unsafe-finally": "error",
      "no-unsafe-optional-chaining": "error",
      "use-isnan": "error",
      "valid-typeof": "error",
      eqeqeq: ["error", "always", { null: "ignore" }],
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
];
