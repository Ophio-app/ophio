// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import tauri from "../../../src-tauri/tauri.conf.json";
import { loadNotices, protectionWords } from "./Versions";

function reply(body: string, status = 200, type = "text/plain") {
  return new Response(body, { status, headers: { "content-type": type } });
}

describe("loadNotices", () => {
  it("joins the product notice and the third-party notices", async () => {
    const asked: string[] = [];
    const text = await loadNotices(async (url) => {
      asked.push(String(url));
      return reply(String(url).endsWith("NOTICE") ? "Notice\n" : "Third party\n");
    }, "/");
    expect(asked).toEqual(["/licenses/NOTICE", "/licenses/THIRD-PARTY-NOTICES.txt"]);
    expect(text).toBe("Notice\n\nThird party");
  });

  it("reports a build without notices, including a server's fallback page", async () => {
    expect(await loadNotices(async () => reply("", 404), "/")).toBeNull();
    expect(
      await loadNotices(async () => reply("<!doctype html>", 200, "text/html"), "/"),
    ).toBeNull();
  });
});

describe("the app's content security policy", () => {
  const directive = (name: string) =>
    tauri.app.security.csp
      .split(";")
      .map((part) => part.trim().split(/\s+/))
      .find(([key]) => key === name)
      ?.slice(1);

  // Settings reads the notices from the app's own bundle; nothing else is reachable.
  it("lets the app read its bundled notices and nothing outside it", () => {
    expect(directive("connect-src")).toEqual(["'self'", "ipc:", "http://ipc.localhost"]);
  });

  // The bundler inlines the smallest font subsets into the stylesheet as data: URLs.
  it("loads the bundled fonts, inlined ones included, and no others", () => {
    expect(directive("font-src")).toEqual(["'self'", "data:"]);
  });
});

describe("protectionWords", () => {
  it("says how the device key is kept in plain words", () => {
    for (const value of ["keystore", "wrapped"]) {
      const words = protectionWords(value);
      expect(words).not.toMatch(/Keystore|wrapping/);
      expect(words).toMatch(/secure storage/);
    }
  });
});
