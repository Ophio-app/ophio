// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { blobs } from "../../state/annotations";
import { chooseComposerFile, composerFileKey, transferComposer } from "../transfers/composer";
import { uuidv7 } from "../../protocol/ids";
import { transferBytes } from "../transfers/storage";
import Inbox from "./Inbox";
import { inboxStore } from "./inbox";

vi.mock("../transfers/storage", () => {
  const files = new Map<string, Blob>();
  return {
    transferBytes: {
      put: async (key: string, blob: Blob) => { files.set(key, blob); },
      get: async (key: string) => files.get(key) ?? null,
      remove: async (key: string) => { files.delete(key); },
    },
  };
});

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const saved = () => JSON.parse(localStorage.getItem(`transfer-composer.${app.profile.profileId}`) ?? "null");
const show = () => render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "inbox" }, depth: 1, active: true }}><Inbox route={{ screen: "inbox" }} /></RouteProvider></AppProvider>);
const sendToComputer = async () => {
  fireEvent.click(screen.getByRole("button", { name: "Send to computer…" }));
  await advance();
};
beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const stored = new Map<string, Blob>();
  vi.spyOn(blobs, "put").mockImplementation(async (blob) => { const key = `blob-${stored.size}`; stored.set(key, blob); return key; });
  vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
  vi.spyOn(blobs, "remove").mockImplementation(async (key) => { stored.delete(key); });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start();
  await advance(900);
  app.ui.push({ screen: "inbox" }, "work");
  // The composer is shared for the profile; start each test with it empty.
  transferComposer(profile.profileId).update({ mode: "file", text: "", name: "note.md", fileName: "", fileSize: 0, fileId: undefined, submitted: undefined });
  await transferBytes.remove(composerFileKey(profile.profileId));
  const inbox = inboxStore(profile.profileId);
  for (const item of inbox.get().items) await inbox.discard(item.id);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
const shareText = () =>
  inboxStore(app.profile.profileId).add({ kind: "text", text: "Shared idea", blobKey: null, name: "Idea", mediaType: "text/plain", size: 11, sourceApp: null, projectId: null });
const shareFile = () =>
  inboxStore(app.profile.profileId).addBlob(new Blob(["shared bytes"], { type: "application/pdf" }), { kind: "file", name: "shared.pdf", sourceApp: null, projectId: null });
const chooseFile = async (text: string, fileName: string) => {
  const fileId = uuidv7(), blob = new Blob([text]);
  await transferBytes.put(composerFileKey(app.profile.profileId, fileId), blob);
  chooseComposerFile(app.profile.profileId, { fileId, fileName, fileSize: blob.size });
  return fileId;
};

it("asks before a shared text replaces an unsent note, and cancelling changes nothing", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "My unsent note", name: "plan.md" });
  const before = saved();
  shareText();
  show();
  await sendToComputer();
  expect(screen.getByRole("heading", { name: "Transfers has an unsent note" })).toBeTruthy();
  expect(saved()).toEqual(before);
  expect(app.ui.route()).toEqual({ screen: "inbox" });
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await advance();
  expect(saved()).toEqual(before);
  expect(composer.get()).toMatchObject({ mode: "text", text: "My unsent note", name: "plan.md" });
  expect(app.ui.route()).toEqual({ screen: "inbox" });
  // The note survives a restart: the saved copy was never touched.
  expect(saved()).toMatchObject({ text: "My unsent note", name: "plan.md" });
});

it("asks about an unsent note whose file name was cleared", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "My unsent note", name: "" });
  const before = saved();
  shareText();
  show();
  await sendToComputer();
  expect(screen.getByRole("heading", { name: "Transfers has an unsent note" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Replace the unnamed note" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await advance();
  expect(composer.get()).toMatchObject({ mode: "text", text: "My unsent note", name: "" });
  expect(saved()).toEqual(before);
  expect(app.ui.route()).toEqual({ screen: "inbox" });
});

it("opens Transfers on the unsent note when the person keeps it", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "My unsent note", name: "plan.md" });
  shareText();
  show();
  await sendToComputer();
  fireEvent.click(screen.getByRole("button", { name: "Keep plan.md and open Transfers" }));
  await advance();
  expect(composer.get()).toMatchObject({ mode: "text", text: "My unsent note", name: "plan.md" });
  expect(app.ui.route()).toEqual({ screen: "transfers" });
});

it("replaces the unsent note only when the person chooses Replace", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "My unsent note", name: "plan.md" });
  shareText();
  show();
  await sendToComputer();
  fireEvent.click(screen.getByRole("button", { name: "Replace plan.md" }));
  await advance();
  expect(composer.get()).toMatchObject({ mode: "text", text: "Shared idea", name: "Idea.md" });
  expect(saved()).toMatchObject({ text: "Shared idea", name: "Idea.md" });
  expect(app.ui.route()).toEqual({ screen: "transfers" });
});

it("asks before a shared file replaces the chosen file, and cancelling keeps its bytes", async () => {
  const composer = transferComposer(app.profile.profileId);
  const key = composerFileKey(app.profile.profileId);
  const chosen = new File(["quarterly report"], "report.pdf");
  await transferBytes.put(key, chosen);
  composer.update({ mode: "file", fileName: "report.pdf", fileSize: chosen.size });
  const before = saved();
  await shareFile();
  show();
  await sendToComputer();
  expect(screen.getByRole("heading", { name: "Transfers has an unsent file" })).toBeTruthy();
  expect(await transferBytes.get(key)).toBe(chosen);
  expect(saved()).toEqual(before);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await advance();
  expect(await transferBytes.get(key)).toBe(chosen);
  expect(saved()).toEqual(before);
  expect(app.ui.route()).toEqual({ screen: "inbox" });

  await sendToComputer();
  fireEvent.click(screen.getByRole("button", { name: "Replace report.pdf" }));
  await advance();
  const fileId = composer.get().fileId;
  expect(fileId).toBeTruthy();
  expect(await (await transferBytes.get(composerFileKey(app.profile.profileId, fileId)))!.text()).toBe("shared bytes");
  expect(await transferBytes.get(key)).toBeNull();
  expect(composer.get()).toMatchObject({ mode: "file", fileName: "shared.pdf", fileSize: 12 });
  expect(app.ui.route()).toEqual({ screen: "transfers" });
});

it("hands off without asking when the slot it fills is empty, keeping the other slot", async () => {
  const composer = transferComposer(app.profile.profileId);
  const key = composerFileKey(app.profile.profileId);
  const chosen = new File(["quarterly report"], "report.pdf");
  await transferBytes.put(key, chosen);
  composer.update({ mode: "file", fileName: "report.pdf", fileSize: chosen.size });
  shareText();
  show();
  await sendToComputer();
  expect(screen.queryByRole("heading", { name: /Transfers has an unsent/ })).toBeNull();
  expect(composer.get()).toMatchObject({ mode: "text", text: "Shared idea", fileName: "report.pdf" });
  expect(await transferBytes.get(key)).toBe(chosen);
  expect(app.ui.route()).toEqual({ screen: "transfers" });
});

it("doesn't ask again for the same item already in Transfers", async () => {
  shareText();
  const { unmount } = show();
  await sendToComputer();
  expect(app.ui.route()).toEqual({ screen: "transfers" });
  unmount();
  app.ui.back();
  show();
  await sendToComputer();
  expect(screen.queryByRole("heading", { name: /Transfers has an unsent/ })).toBeNull();
  expect(app.ui.route()).toEqual({ screen: "transfers" });
});

it("doesn't ask again for the same shared file already chosen in Transfers", async () => {
  await shareFile();
  const { unmount } = show();
  await sendToComputer();
  expect(app.ui.route()).toEqual({ screen: "transfers" });
  unmount();
  app.ui.back();
  show();
  await sendToComputer();
  // Comparing the bytes takes real time, not the fake timers'.
  await vi.waitFor(() => expect(app.ui.route()).toEqual({ screen: "transfers" }));
  expect(screen.queryByRole("heading", { name: /Transfers has an unsent/ })).toBeNull();
});

it("doesn't ask about a note Send already took", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "Sent note", name: "sent.md" });
  composer.submitted("job");
  shareText();
  show();
  await sendToComputer();
  expect(screen.queryByRole("heading", { name: /Transfers has an unsent/ })).toBeNull();
  expect(composer.get()).toMatchObject({ text: "Shared idea", name: "Idea.md" });
});

it("asks again when the note changes after the question", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "My unsent note", name: "plan.md" });
  shareText();
  show();
  await sendToComputer();
  // Transfers in another tab keeps editing the note.
  act(() => composer.update({ text: "My unsent note, longer", name: "plan-2.md" }));
  fireEvent.click(screen.getByRole("button", { name: "Replace plan.md" }));
  await advance();
  expect(composer.get()).toMatchObject({ text: "My unsent note, longer", name: "plan-2.md" });
  expect(screen.getByRole("button", { name: "Replace plan-2.md" })).toBeTruthy();
  expect(app.ui.route()).toEqual({ screen: "inbox" });
});

it("asks again when an inactive file changes without changing the text revision", async () => {
  const composer = transferComposer(app.profile.profileId);
  composer.update({ mode: "text", text: "Keep this note" });
  await chooseFile("first file", "first.pdf");
  await shareFile();
  show();
  await sendToComputer();
  const revision = composer.get().revision;
  const fileId = await chooseFile("second file", "second.pdf");
  expect(composer.get().revision).toBe(revision);
  fireEvent.click(screen.getByRole("button", { name: "Replace first.pdf" }));
  await advance();
  expect(composer.get()).toMatchObject({ mode: "text", text: "Keep this note", fileId, fileName: "second.pdf" });
  expect(screen.getByRole("button", { name: "Replace second.pdf" })).toBeTruthy();
  expect(await (await transferBytes.get(composerFileKey(app.profile.profileId, fileId)))!.text()).toBe("second file");
  expect(app.ui.route()).toEqual({ screen: "inbox" });
});

it("checks the current draft again after comparing an earlier file's bytes", async () => {
  const composer = transferComposer(app.profile.profileId);
  await chooseFile("shared bytes", "shared.pdf");
  await shareFile();
  const get = transferBytes.get;
  let release = () => {};
  vi.spyOn(transferBytes, "get").mockImplementationOnce(async (key) => {
    const held = await get(key);
    return new Promise((resolve) => { release = () => resolve(held); });
  });
  show();
  await sendToComputer();
  const fileId = await chooseFile("new draft", "new.pdf");
  await act(async () => { release(); });
  await vi.waitFor(() => expect(screen.getByRole("button", { name: "Replace new.pdf" })).toBeTruthy());
  expect(composer.get()).toMatchObject({ fileId, fileName: "new.pdf" });
  expect(await (await transferBytes.get(composerFileKey(app.profile.profileId, fileId)))!.text()).toBe("new draft");
  expect(app.ui.route()).toEqual({ screen: "inbox" });
});

it("asks about a file chosen while the shared file is being stored", async () => {
  const composer = transferComposer(app.profile.profileId);
  await shareFile();
  const put = transferBytes.put;
  let finish = async () => {};
  let stagedKey = "";
  vi.spyOn(transferBytes, "put").mockImplementationOnce((key, blob) => new Promise((resolve) => {
    stagedKey = key;
    finish = async () => { await put(key, blob); resolve(undefined); };
  }));
  show();
  await sendToComputer();
  const fileId = await chooseFile("chosen meanwhile", "later.pdf");
  await act(finish);
  await advance();
  expect(composer.get()).toMatchObject({ fileId, fileName: "later.pdf" });
  expect(screen.getByRole("button", { name: "Replace later.pdf" })).toBeTruthy();
  expect(await transferBytes.get(stagedKey)).toBeNull();
  expect(await (await transferBytes.get(composerFileKey(app.profile.profileId, fileId)))!.text()).toBe("chosen meanwhile");
  expect(app.ui.route()).toEqual({ screen: "inbox" });
});
