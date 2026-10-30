// SPDX-License-Identifier: Apache-2.0
import type { Handle } from "mdast-util-from-markdown";
import { decodeString } from "micromark-util-decode-string";
import type {} from "remark-parse";
import type { Plugin } from "unified";

// Read the destination token, not the whole Markdown document: code, labels,
// titles and non-Windows destinations must keep their normal escape rules.
const destination: Handle = function (token) {
  const decoded = this.resume();
  const node = this.stack.at(-1);
  if (!node || !("url" in node)) return;
  const raw = this.sliceSerialize(token);
  // In a backslash drive path, separators stay even before punctuation
  // such as `.config` or `_report`. Protect them from Markdown unescaping,
  // while still decoding character references such as &amp; normally.
  // Doubled/escaped backslashes name the same Windows separator.
  node.url = /^[a-z]:\\/i.test(raw)
    ? decodeString(raw.replace(/\\+/g, "%5C"))
    : decoded;
};

export const remarkWindowsFileLinks: Plugin = function () {
  const data = this.data();
  const extensions = data.fromMarkdownExtensions ??= [];
  extensions.push({
    exit: {
      resourceDestinationString: destination,
      definitionDestinationString: destination,
    },
  });
};
