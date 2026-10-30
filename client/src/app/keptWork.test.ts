// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { HostProfile } from "../host/link";
import { carryOver, keepWork } from "./keptWork";
import { transferBytes } from "../screens/transfers/storage";
import { previewDrafts } from "../screens/control/previewDrafts";
import { platformProblems } from "../platform/errors";

vi.mock("../screens/transfers/storage", () => ({ transferBytes: { move: vi.fn(async () => {}) } }));
vi.mock("../screens/control/previewDrafts", () => ({ previewDrafts: { move: vi.fn(async () => {}) } }));

const computer = (profileId: string, hostId = "host-1"): HostProfile =>
  ({ profileId, hostId, name: "studio", addresses: [], certSha256: "a".repeat(64), pairedAt: 1, deviceId: `device-${profileId}` });

beforeEach(() => {
  localStorage.clear();
  platformProblems.set([]);
  vi.stubGlobal("indexedDB", {});
  vi.mocked(transferBytes.move).mockClear().mockResolvedValue(undefined);
  vi.mocked(previewDrafts.move).mockClear().mockResolvedValue(undefined);
});
afterEach(() => vi.unstubAllGlobals());

it("moves a replaced pairing's work, but not its command history, to the new pairing", async () => {
  localStorage.setItem("ui.old-1", JSON.stringify({ version: 2, state: { drafts: { "p/new": { text: "unsent" } } } }));
  localStorage.setItem("work.old-1.tasks.search.p", "\"query\"");
  localStorage.setItem("transfers.v1.old-1", JSON.stringify({ t1: { profileId: "old-1" } }));
  localStorage.setItem("command-history.old-1", JSON.stringify({ id: "h", head: 9 }));
  localStorage.setItem("ui.other", "theirs");
  await carryOver(computer("new-1"), ["old-1"]);
  expect(JSON.parse(localStorage.getItem("ui.new-1")!).state.drafts["p/new"].text).toBe("unsent");
  expect(localStorage.getItem("work.new-1.tasks.search.p")).toBe("\"query\"");
  // References to the old pairing inside what moved name the new one.
  expect(JSON.parse(localStorage.getItem("transfers.v1.new-1")!)).toEqual({ t1: { profileId: "new-1" } });
  expect(localStorage.getItem("command-history.new-1")).toBeNull();
  expect(Object.keys(localStorage).filter((k) => k.includes("old-1"))).toEqual(["command-history.old-1"]);
  expect(localStorage.getItem("ui.other")).toBe("theirs");
  expect(transferBytes.move).toHaveBeenCalledWith("old-1", "new-1");
  expect(previewDrafts.move).toHaveBeenCalledWith("old-1", "new-1");
});

it("gives a forgotten computer's work only to a new pairing of that computer", async () => {
  localStorage.setItem("ui.old-1", "mine");
  localStorage.setItem("ui.old-2", "another computer's");
  keepWork(computer("old-1"));
  keepWork(computer("old-2", "host-2"));
  await carryOver(computer("new-3", "host-3"), []);
  expect(localStorage.getItem("ui.new-3")).toBeNull();
  await carryOver(computer("new-1"), []);
  expect(localStorage.getItem("ui.new-1")).toBe("mine");
  expect(localStorage.getItem("ui.old-2")).toBe("another computer's");
  // Only host-2's note remains, for when it is paired again.
  await carryOver(computer("new-2", "host-2"), []);
  expect(localStorage.getItem("ui.new-2")).toBe("another computer's");
  expect(localStorage.getItem("kept-work.v1")).toBeNull();
});

it("keeps what the new pairing already has and prefers the newer earlier pairing", async () => {
  localStorage.setItem("ui.old-1", "older");
  localStorage.setItem("ui.old-2", "newer");
  localStorage.setItem("voice.new-1", "already set");
  localStorage.setItem("voice.old-2", "replaced");
  keepWork(computer("old-1"));
  await carryOver(computer("new-1"), ["old-2"]);
  expect(localStorage.getItem("ui.new-1")).toBe("newer");
  expect(localStorage.getItem("voice.new-1")).toBe("already set");
});

it("keeps a note of work that couldn't move and says so", async () => {
  localStorage.setItem("ui.old-1", "unsent");
  vi.mocked(transferBytes.move).mockRejectedValueOnce(new Error("full"));
  await carryOver(computer("new-1"), ["old-1"]);
  expect(localStorage.getItem("ui.old-1")).toBe("unsent");
  expect(platformProblems.get().map((p) => p.message).join()).toMatch(/couldn’t move to the new pairing/);
  // The next pairing of this computer tries the files again.
  await carryOver(computer("new-2"), ["new-1"]);
  expect(transferBytes.move).toHaveBeenCalledWith("old-1", "new-2");
  expect(localStorage.getItem("ui.new-2")).toBe("unsent");
});

it("leaves a value where it was when storage refuses the moved copy", async () => {
  localStorage.setItem("ui.old-1", "unsent");
  const setItem = Storage.prototype.setItem;
  const refuse = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key: string, value: string) {
    if (key === "ui.new-1") throw new DOMException("full", "QuotaExceededError");
    setItem.call(this, key, value);
  });
  await carryOver(computer("new-1"), ["old-1"]);
  refuse.mockRestore();
  expect(localStorage.getItem("ui.old-1")).toBe("unsent");
  expect(JSON.parse(localStorage.getItem("kept-work.v1")!)).toMatchObject([{ profileId: "old-1", hostId: "host-1" }]);
});

it("never holds up the new pairing when storage can't be used", async () => {
  localStorage.setItem("ui.old-1", "unsent");
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new DOMException("denied", "SecurityError"); });
  await expect(carryOver(computer("new-1"), ["old-1"])).resolves.toBeUndefined();
  read.mockRestore();
  expect(localStorage.getItem("ui.old-1")).toBe("unsent");
  expect(platformProblems.get()).toHaveLength(1);
});

it("does not rewrite a profile ID inside a draft or delete a conflicting older value", async () => {
  const draft = JSON.stringify({ version: 2, state: { drafts: { text: "Mention old-1 verbatim" } } });
  localStorage.setItem("ui.old-1", draft);
  await carryOver(computer("new-1"), ["old-1"]);
  expect(localStorage.getItem("ui.new-1")).toBe(draft);
  localStorage.setItem("ui.old-2", "another draft");
  await carryOver(computer("new-1"), ["old-2"]);
  expect(localStorage.getItem("ui.old-2")).toBe("another draft");
  expect(localStorage.getItem("ui.new-1")).toBe(draft);
});
