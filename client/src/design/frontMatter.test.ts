// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { splitFrontMatter } from "./frontMatter";

describe("splitFrontMatter", () => {
  it("separates flat fields from the document body", () => {
    const { front, body } = splitFrontMatter(
      '---\nname: checkout-polish\ndescription: "Draft for review: from a receipt."\n---\n# Checkout polish\n',
    );
    expect(front?.fields).toEqual([
      ["name", "checkout-polish"],
      ["description", "Draft for review: from a receipt."],
    ]);
    expect(body).toBe("# Checkout polish\n");
  });

  it("keeps nested YAML as source", () => {
    const { front, body } = splitFrontMatter(
      "---\nname: a\ntools:\n  - Read\n  - Edit\n---\nBody",
    );
    expect(front?.fields).toBeNull();
    expect(front?.raw).toBe("name: a\ntools:\n  - Read\n  - Edit");
    expect(body).toBe("Body");
  });

  it("handles CRLF, a byte order mark and a file that is only front matter", () => {
    const { front, body } = splitFrontMatter("﻿---\r\nname: a\r\n---");
    expect(front?.fields).toEqual([["name", "a"]]);
    expect(body).toBe("");
  });

  it("leaves text without a leading fence alone", () => {
    for (const text of [
      "# Title\n---\nname: a\n---\n",
      "Intro\n\n---\n",
      "---\nname: a\nno closing fence",
    ]) {
      expect(splitFrontMatter(text)).toEqual({ front: null, body: text });
    }
  });
});
