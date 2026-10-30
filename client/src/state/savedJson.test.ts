// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it, vi } from "vitest";
import { readSavedJson, UnpreservedJson } from "./savedJson";

function storage() {
  const data = new Map<string, string>();
  return {
    data,
    getItem: vi.fn((key: string) => data.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => { data.set(key, value); }),
  };
}

describe("saved JSON recovery", () => {
  it.each([null, "null", '{"draft":"hello"}'])("reads %s without a recovery write", (text) => {
    const s = storage();
    if (text !== null) s.data.set("key", text);
    expect(readSavedJson(s, "key")).toEqual({ value: JSON.parse(text ?? "null"), recovered: false });
    expect(s.setItem).not.toHaveBeenCalled();
  });

  it.each(["{not json", "", '{"draft":"keep 🌱\n'])("preserves the exact damaged value %j before starting fresh", (text) => {
    const s = storage();
    s.data.set("key", text);
    expect(readSavedJson(s, "key")).toEqual({ value: null, recovered: true });
    expect(s.data.get("key.unreadable")).toBe(text);
    expect(s.data.get("key")).toBe(text);
  });

  it("reuses a backup across restarts and preserves every distinct damaged value", () => {
    const s = storage();
    for (const text of ["{first", "{second", "{third"]) {
      s.data.set("key", text);
      readSavedJson(s, "key");
      readSavedJson(s, "key");
    }
    expect(s.setItem.mock.calls).toEqual([
      ["key.unreadable", "{first"],
      ["key.unreadable.1", "{second"],
      ["key.unreadable.2", "{third"],
    ]);
  });

  it("keeps profiles and stores separate", () => {
    const s = storage();
    for (const key of ["library.v1.p", "library.v1.other", "transfers.v1.p"]) {
      s.data.set(key, `{${key}`);
      readSavedJson(s, key);
      expect(s.data.get(`${key}.unreadable`)).toBe(`{${key}`);
    }
    expect(s.data.size).toBe(6);
  });

  it("does not mistake a read error for damaged JSON, even if it is a SyntaxError", () => {
    const s = storage(), error = new SyntaxError("Read unavailable");
    s.getItem.mockImplementation(() => { throw error; });
    expect(() => readSavedJson(s, "key")).toThrow(error);
    expect(s.setItem).not.toHaveBeenCalled();
  });

  it.each(["read", "write"])("stays blocked if the backup %s fails", (operation) => {
    const s = storage();
    s.data.set("key", "{broken");
    if (operation === "read") s.getItem.mockImplementation((key) => {
      if (key !== "key") throw new Error("Unavailable");
      return s.data.get(key) ?? null;
    });
    else s.setItem.mockImplementation(() => { throw new Error("Quota exceeded"); });
    expect(() => readSavedJson(s, "key")).toThrow(UnpreservedJson);
    expect([...s.data.entries()]).toEqual([["key", "{broken"]]);
  });
});
