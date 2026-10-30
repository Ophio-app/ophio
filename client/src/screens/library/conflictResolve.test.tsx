// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, renderHook, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Conflict from "./Conflict";
import { setNavBack } from "../../design/back";
import { fileKey, LibraryStore, libraryStore } from "./drafts";
import File from "./File";
import { useFolder } from "./shared";
import { checkSave, compareWithComputer, saveDraft } from "./save";
vi.mock("./Editor", () => ({ default: ({ initialText, onChange }: { initialText: string; onChange: (text: string) => void }) => <textarea aria-label="Markdown source" defaultValue={initialText} onChange={event => onChange(event.target.value)} /> }));
let app: App,
  serial = 0;
const advance = async (ms = 500) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider
      value={{
        tab: "library",
        route: { screen: "library" },
        depth: 1,
        active: true,
      }}
    >
      {children}
    </RouteProvider>
  </AppProvider>
);
function host() {
  if (!window.__sim) throw new Error("Missing simulated host");
  return window.__sim.host;
}
async function run<T>(operation: () => Promise<T>): Promise<T> {
  let result: T;
  await act(async () => {
    const p = operation();
    await vi.advanceTimersByTimeAsync(900);
    result = await p;
  });
  return result!;
}
beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  // Deterministic fixture revisions avoid native crypto worker scheduling under fake time.
  vi.spyOn(crypto.subtle, "digest").mockImplementation(
    async (_algorithm, data) => {
      const bytes = ArrayBuffer.isView(data)
        ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
        : new Uint8Array(data);
      const hash = new Uint8Array(32);
      bytes.forEach((value, i) => {
        hash[i % 32] = ((hash[i % 32] ?? 0) * 31 + value) % 256;
      });
      return hash.buffer;
    },
  );
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true;
  };
  HTMLDialogElement.prototype.close = function () {
    this.open = false;
  };
  const platform = createSimPlatform(),
    profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing profile");
  const session = new HostSession(
    profile.profileId,
    platform.control(profile.profileId),
  );
  app = {
    platform,
    profile: { ...profile, profileId: `conflict-${serial++}` },
    profiles: [profile],
    session,
    ui: new UiStore(profile.profileId),
    switchTo: vi.fn(),
    pairAnother: vi.fn(),
    rename: vi.fn(),
    forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  app.session.stop();
  host().dispose();
  app.ui.persistNow();
  await advance(20);
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const route = { screen: "file", rootId: ids.rootInstr, path: "docs/windows.md" } as const;
const mine = "# Windows notes (mine)\n\nKeep the CRLF line endings.\nMine at the end\n";
const theirs = "# Windows notes (theirs)\r\n\r\nKeep the line endings as they are.\r\n";

function limitLibraryStorage(store: LibraryStore) {
  const setItem = Storage.prototype.setItem;
  const getItem = Storage.prototype.getItem;
  let saved: string | null = null;
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(function (this: Storage, key) {
    return key === store.key ? saved : getItem.call(this, key);
  });
  const attempts: number[] = [];
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (key === store.key) {
      attempts.push(value.length);
      if (key.length + value.length > 5 * 1024 * 1024) throw new DOMException("Quota", "QuotaExceededError");
      saved = value;
      return;
    }
    setItem.call(this, key, value);
  });
  return attempts;
}

it("keeps a newly read file visible and editable when the read evicts persisted caches", async () => {
  const store = libraryStore(app.profile.profileId);
  const attempts = limitLibraryStorage(store);
  const text = "# Read survives eviction\n\n" + "x".repeat(500 * 1024 - 26);
  host().files[route.rootId]![route.path] = text;
  for (let i = 0; i < 10; i++) store.cacheFile({
    root_id: route.rootId, path: `cached-${i}.md`, text, size: text.length,
    revision: `rev-${i}`, encoding: "utf8", newline: "lf", has_bom: false, modified_at: 1,
  });
  render(<File route={route} />, { wrapper });
  await advance(1000);
  expect(attempts.some(size => size > 5 * 1024 * 1024)).toBe(true);
  expect(screen.queryByText("Reading file…")).toBeNull();
  expect(screen.getByRole("heading", { name: "Read survives eviction" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  expect((screen.getByLabelText("Markdown source") as HTMLTextAreaElement).value).toBe(text);
  attempts.length = 0;
  fireEvent.change(screen.getByLabelText("Markdown source"), { target: { value: "edited" } });
  expect(attempts).toHaveLength(1);
  // The large draft base is necessary; discarded read caches are not included.
  expect(attempts[0]).toBeLessThan(600 * 1024);
  expect(new LibraryStore(app.profile.profileId).draft(route.rootId, route.path)?.text).toBe("edited");
});

it("keeps a newly read folder available when its listing evicts persisted caches", async () => {
  const store = libraryStore(app.profile.profileId);
  const attempts = limitLibraryStorage(store);
  const text = "x".repeat(500 * 1024);
  for (let i = 0; i < 10; i++) store.cacheFile({
    root_id: route.rootId, path: `cached-${i}.md`, text, size: text.length,
    revision: `rev-${i}`, encoding: "utf8", newline: "lf", has_bom: false, modified_at: 1,
  });
  for (let i = 0; i < 700; i++) host().files[route.rootId]![`quota-folder/${i}-${"n".repeat(100)}.md`] = "note";
  attempts.length = 0;
  const { result } = renderHook(() => useFolder(route.rootId, "quota-folder"), { wrapper });
  await advance(1000);
  expect(attempts).toHaveLength(2);
  expect(attempts[0]).toBeGreaterThan(5 * 1024 * 1024);
  expect(result.current.cached?.value.entries).toHaveLength(700);
  expect(result.current.loading).toBe(false);
  expect(result.current.error).toBeNull();
  attempts.length = 0;
  act(() => store.form("short-edit", "kept"));
  expect(attempts).toHaveLength(1);
  expect(attempts[0]).toBeLessThan(4096);
  expect(result.current.cached?.value.entries).toHaveLength(700);
});

it("shows the computer's file and the conflict after a save conflict, and says what Review compares", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  // The folder says which windows.md this is.
  expect(screen.getByText("tidewater-api › docs")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const store = libraryStore(app.profile.profileId);
  act(() => store.saveDraft({ ...store.draft(route.rootId, route.path)!, text: mine }));
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, theirs);
  });
  fireEvent.click(within(screen.getByRole("dialog", { name: "Edit Markdown" })).getByRole("button", { name: "Review changes" }));
  const review = within(screen.getByRole("dialog", { name: "Review save" }));
  expect(review.getByText(/Compared with the version you opened/)).toBeTruthy();
  fireEvent.click(review.getByRole("button", { name: "Save on computer" }));
  await advance(1000);
  expect(store.draft(route.rootId, route.path)?.phase).toBe("conflict");
  const note = document.querySelector(".lib-document")!;
  expect(note.textContent).toContain("Windows notes (theirs)");
  expect(note.textContent).not.toContain("Keep the line endings.");
  expect(screen.getByText("Conflict · changed on the computer")).toBeTruthy();
  expect(screen.getByText(/^Showing what is on the computer now/)).toBeTruthy();
  expect(screen.queryByText("Changed on the computer since you opened it")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Continue editing draft" }));
  await advance();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Edit Markdown" })).getByRole("button", { name: "Review changes" }));
  const again = within(screen.getByRole("dialog", { name: "Review save" }));
  expect(again.getByText("Conflict · changed on the computer")).toBeTruthy();
  expect(again.getByText(/Compared with the version you opened/)).toBeTruthy();
  expect(again.queryByText(/Showing what is on the computer now/)).toBeNull();
});

it("brings a newer computer version to Review before saving, and to a plain reader on request", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  const store = libraryStore(app.profile.profileId);
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, theirs);
  });
  await advance();
  expect(screen.getByText("Changed on the computer since you opened it")).toBeTruthy();
  expect(document.querySelector(".lib-document")!.textContent).not.toContain("Windows notes (theirs)");
  fireEvent.click(screen.getByRole("button", { name: "Show the latest" }));
  await advance(1000);
  expect(document.querySelector(".lib-document")!.textContent).toContain("Windows notes (theirs)");
  expect(screen.queryByText("Changed on the computer since you opened it")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  act(() => store.saveDraft({ ...store.draft(route.rootId, route.path)!, text: mine }));
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, "# Windows notes (newer)\r\n");
  });
  await advance();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Edit Markdown" })).getByRole("button", { name: "Review changes" }));
  const review = within(screen.getByRole("dialog", { name: "Review save" }));
  expect(review.getByText("Changed on the computer since you opened it")).toBeTruthy();
  expect(review.getByText(/Your draft starts from an earlier version/)).toBeTruthy();
  fireEvent.click(review.getByRole("button", { name: "Compare with the computer’s" }));
  await advance(1000);
  expect(store.draft(route.rootId, route.path)).toMatchObject({ phase: "conflict", text: mine, current: { text: "# Windows notes (newer)\r\n" } });
  expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toMatchObject({ screen: "conflict", path: route.path });
  expect(screen.queryByRole("dialog", { name: "Review save" })).toBeNull();
});

it("keeps text typed while Compare reads the computer's version, and leaves a draft discarded meanwhile alone", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  const store = libraryStore(app.profile.profileId);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  act(() => store.saveDraft({ ...store.draft(route.rootId, route.path)!, text: mine }));
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, theirs);
  });
  await advance();
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  fireEvent.click(editor.getByRole("button", { name: "Compare with the computer’s" }));
  // The person keeps typing before the computer answers.
  act(() => store.saveDraft({ ...store.draft(route.rootId, route.path)!, text: `${mine}Typed during the read\n` }));
  await advance(1000);
  expect(store.draft(route.rootId, route.path)).toMatchObject({
    phase: "conflict",
    text: `${mine}Typed during the read\n`,
    current: { text: theirs },
  });

  // A draft discarded while the read is pending stays discarded.
  act(() => store.saveDraft({ ...store.draft(route.rootId, route.path)!, phase: "draft", current: undefined }));
  const before = app.ui.get().stacks[app.ui.get().tab].length;
  let result: string | undefined;
  await act(async () => {
    const p = compareWithComputer(app.session, store, store.draft(route.rootId, route.path)!);
    store.discardDraft(route.rootId, route.path);
    await vi.advanceTimersByTimeAsync(900);
    result = await p;
  });
  expect(result).toBe("superseded");
  expect(store.draft(route.rootId, route.path)).toBeNull();
  expect(app.ui.get().stacks[app.ui.get().tab].length).toBe(before);
});

it("leaves a draft started again from the same version alone when an earlier Compare answers", async () => {
  const store = libraryStore(app.profile.profileId);
  const opened = await run(() => app.session.query({ kind: "library_read", root_id: route.rootId, path: route.path }));
  const view = render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const first = store.draft(route.rootId, route.path)!;
  // Compare reads the computer's newer version, and that answer is slow.
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, theirs);
  });
  const newer = await run(() => app.session.query({ kind: "library_read", root_id: route.rootId, path: route.path }));
  let answer: (v: typeof newer) => void = () => undefined;
  vi.spyOn(app.session, "query").mockImplementationOnce(() => new Promise((resolve) => { answer = resolve as typeof answer; }));
  const comparing = compareWithComputer(app.session, store, first);
  // Meanwhile the computer went back, the person discarded that draft,
  // reopened the file and started a new draft from the same version.
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, opened.file.text);
  });
  act(() => store.discardDraft(route.rootId, route.path));
  view.unmount();
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const second = store.draft(route.rootId, route.path)!;
  expect(second.base.revision).toBe(first.base.revision);
  expect(second.startedAt).toBeGreaterThan(first.startedAt!);
  expect(new LibraryStore(app.profile.profileId).draft(route.rootId, route.path)?.startedAt).toBe(second.startedAt);
  let result: string | undefined;
  await act(async () => {
    answer(newer);
    result = await comparing;
  });
  expect(result).toBe("superseded");
  expect(store.draft(route.rootId, route.path)).toMatchObject({ phase: "draft", startedAt: second.startedAt });
  expect(store.draft(route.rootId, route.path)?.current).toBeUndefined();
  expect(store.get().files[fileKey(route.rootId, route.path)]?.value.revision).toBe(opened.file.revision);
});

it("checks an unconfirmed save against the draft as it is after the read", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  const store = libraryStore(app.profile.profileId);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const opened = store.draft(route.rootId, route.path)!;
  act(() => store.saveDraft({ ...opened, text: mine, phase: "unknown" }));
  let result: Awaited<ReturnType<typeof checkSave>> | undefined;
  await act(async () => {
    const p = checkSave(app.session, store, store.draft(route.rootId, route.path)!);
    store.saveDraft({ ...store.draft(route.rootId, route.path)!, text: `${mine}More\n` });
    await vi.advanceTimersByTimeAsync(900);
    result = await p;
  });
  // The computer still has the opened version, so the save did not land.
  expect(result).toEqual({ kind: "unchanged" });
  expect(store.draft(route.rootId, route.path)).toMatchObject({ phase: "draft", text: `${mine}More\n` });
});

it("says the file is gone, not the version opened, when a conflict finds it deleted", async () => {
  const store = libraryStore(app.profile.profileId);
  const { file } = await run(() => app.session.query({ kind: "library_read", root_id: route.rootId, path: route.path }));
  store.saveDraft({ base: file, text: mine, phase: "conflict", current: null, currentAt: 1, updatedAt: 1 });
  render(<File route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("Conflict · deleted on the computer")).toBeTruthy();
  expect(document.querySelector(".lib-document")).toBeNull();
  expect(screen.queryByText("Reading file…")).toBeNull();
  expect(screen.getByRole("button", { name: "Review conflicting changes" })).toBeTruthy();
});

it("resolves each overlap on its own and marks only those left to edit by hand", async () => {
  const store = libraryStore(app.profile.profileId);
  const response = await run(() => app.session.query({ kind: "library_read", root_id: route.rootId, path: route.path }));
  store.saveDraft({ base: response.file, text: mine, phase: "draft", updatedAt: 1 });
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, theirs);
  });
  expect((await run(() => saveDraft(app.session, store, store.draft(route.rootId, route.path)!))).kind).toBe("conflict");
  const current = store.draft(route.rootId, route.path)!.current!;
  const view = render(<Conflict route={{ screen: "conflict", rootId: route.rootId, path: route.path }} />, { wrapper });
  await advance();
  const merge = screen.getByRole("button", { name: "Merge into a new draft" });
  expect(merge).toHaveProperty("disabled", true);
  expect(screen.getByText(/Choose what to keep for 2 more overlaps/)).toBeTruthy();
  const [first, second] = screen.getAllByRole("group", { name: /^Keep for overlap/ });
  expect(within(first!.closest("section")!).getByText("# Windows notes (mine)")).toBeTruthy();
  expect(within(first!.closest("section")!).getByText("# Windows notes (theirs)")).toBeTruthy();
  fireEvent.click(within(first!).getByRole("button", { name: "Keep the computer’s" }));
  expect(merge).toHaveProperty("disabled", true);
  fireEvent.click(within(second!).getByRole("button", { name: "Edit by hand" }));
  expect(within(first!).getByRole("button", { name: "Keep the computer’s" }).getAttribute("aria-pressed")).toBe("true");
  expect(merge).toHaveProperty("disabled", false);
  fireEvent.click(merge);
  const merged = store.draft(route.rootId, route.path)!;
  expect(merged).toMatchObject({ phase: "draft", reviewMerge: true, base: { revision: current.revision } });
  expect(merged.text).toBe([
    "# Windows notes (theirs)",
    "",
    "<<<<<<< Draft on this device",
    "Keep the CRLF line endings.",
    "Mine at the end",
    "=======",
    "Keep the line endings as they are.",
    ">>>>>>> Now on the computer",
    "",
  ].join("\r\n"));
  view.unmount();
  render(<File route={route} />, { wrapper });
  await advance(1000);
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  expect(editor.getByText(/Overlaps you chose to edit by hand are between markers/)).toBeTruthy();
  expect(editor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", true);
  expect(document.querySelector(".lib-document")!.textContent).toContain("Windows notes (theirs)");
});

// A note under the 512 KiB read limit can have more lines than a call takes arguments.
const longBase = `a\n${"\n".repeat(150_000)}`, longEdit = `b\n${"\n".repeat(150_000)}`;
it("reviews and saves a note with more lines than a call takes arguments", async () => {
  await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, longBase);
  });
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const store = libraryStore(app.profile.profileId), draft = store.draft(route.rootId, route.path)!;
  expect(draft.base.text).toBe(longBase);
  act(() => store.saveDraft({ ...draft, text: longEdit }));
  fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
  const review = within(screen.getByRole("dialog", { name: "Review save" }));
  expect(review.getByText("+ 1 added · − 1 removed")).toBeTruthy();
  fireEvent.click(review.getByRole("button", { name: "Save on computer" }));
  await advance(1000);
  expect(host().files[route.rootId]![route.path]).toBe(longEdit);
  expect(store.draft(route.rootId, route.path)).toBeNull();
  // The whole File screen with a 150 KB note takes seconds under jsdom.
}, 30_000);
it("reviews and merges a conflict with more lines than a call takes arguments", async () => {
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  const store = libraryStore(app.profile.profileId);
  const file = { ...response.file, newline: "lf" as const };
  store.saveDraft({ base: { ...file, text: longBase }, text: longEdit, phase: "conflict", updatedAt: 1,
    current: { ...file, revision: "computer", text: `a\n${"\n".repeat(150_000)}c` }, currentAt: 1 });
  render(<Conflict route={{ screen: "conflict", rootId: route.rootId, path: route.path }} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("+ 1 added · − 1 removed")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Merge into a new draft" }));
  expect(store.draft(route.rootId, route.path)).toMatchObject({ phase: "draft", reviewMerge: true,
    text: `b\n${"\n".repeat(150_000)}c` });
});


it("asks again about closing a changed editor after system Back dismissed that question", async () => {
  vi.spyOn(history, "pushState").mockImplementation(() => {});
  vi.spyOn(history, "back").mockImplementation(() => window.dispatchEvent(new PopStateEvent("popstate")));
  const navigateBack = vi.fn(),
    systemBack = () => act(() => window.dispatchEvent(new PopStateEvent("popstate"))),
    question = () => screen.queryByRole("dialog", { name: "Keep your device draft?" });
  setNavBack({ canGoBack: () => true, back: navigateBack });
  try {
    render(<File route={route} />, { wrapper });
    await advance(1000);
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    await advance();
    const store = libraryStore(app.profile.profileId), draft = store.draft(route.rootId, route.path);
    if (!draft) throw new Error("Missing draft");
    act(() => store.saveDraft({ ...draft, text: "Changed on the phone\n" }));
    systemBack();
    expect(question()).toBeTruthy();
    systemBack();
    expect(question()).toBeNull();
    systemBack();
    expect(question()).toBeTruthy();
    expect(screen.getByRole("dialog", { name: "Edit Markdown" })).toBeTruthy();
    expect(navigateBack).not.toHaveBeenCalled();
  } finally {
    setNavBack(null);
  }
});

it("keeps the editor open with newer edits after checking confirms the earlier save", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const store = libraryStore(app.profile.profileId), draft = store.draft(route.rootId, route.path)!;
  act(() => store.saveDraft({ ...draft, text: "first edit\n", phase: "unknown", recoveryId: "recovery-1" }));
  let reply!: (value: { kind: "library_read"; file: typeof draft.base }) => void;
  vi.spyOn(app.session, "query").mockImplementationOnce(() => new Promise(resolve => { reply = resolve; }));
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  fireEvent.click(editor.getByRole("button", { name: "Check again" }));
  fireEvent.change(editor.getByLabelText("Markdown source"), { target: { value: "second edit\n" } });
  await act(async () => {
    reply({ kind: "library_read", file: { ...draft.base, text: "first edit\r\n", revision: "saved-hash" } });
  });
  const keptEditor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  expect(keptEditor.getByLabelText("Markdown source")).toHaveProperty("value", "second edit\n");
  expect(keptEditor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", false);
  expect(keptEditor.getByText(/Your newer edits are kept on this device/)).toBeTruthy();
  expect(screen.queryByText("Saved on computer")).toBeNull();
  expect(store.draft(route.rootId, route.path)).toMatchObject({ text: "second edit\n", base: { revision: "saved-hash" }, phase: "draft" });
});

it("keeps the original conflict when independent edits combine beyond the editor limit", async () => {
  const { file } = await run(() => app.session.query({ kind: "library_read", root_id: route.rootId, path: route.path }));
  const base = { ...file, text: "a\nb\nc", newline: "lf" as const };
  const store = libraryStore(app.profile.profileId), text = `${"m".repeat(300_000)}\nb\nc`;
  const current = { ...base, text: `a\nb\n${"t".repeat(300_000)}`, revision: "other", size: 300_004 };
  store.saveDraft({ base, text, current, phase: "conflict", updatedAt: 1 });
  render(<Conflict route={{ ...route, screen: "conflict" }} />, { wrapper });
  const merge = screen.getByRole("button", { name: "Merge into a new draft" });
  expect(merge).toHaveProperty("disabled", true);
  fireEvent.click(merge);
  expect(store.draft(route.rootId, route.path)).toMatchObject({ base, text, phase: "conflict" });
  expect(screen.getByText("Finish this merge on the computer")).toBeTruthy();
  const commands = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Save as a new copy…" }));
  fireEvent.click(screen.getByRole("button", { name: "Create new copy" }));
  await advance(1000);
  expect(commands).toHaveBeenCalledWith(expect.objectContaining({ command: "library_create", text }), expect.anything());
  expect(new LibraryStore(app.profile.profileId).draft(route.rootId, route.path)?.text).toBe(text);
});

it.each(["Overview\n========\nBody", "License\n=======\nApache-2.0"])("saves a Markdown heading underline that only looks like a conflict marker (%j)", async text => {
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  libraryStore(app.profile.profileId).saveDraft({ base: response.file, text, phase: "draft", updatedAt: 1 });
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Continue editing draft" }));
  await advance();
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  expect(editor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", false);
  expect(editor.queryByText("Remove the conflict markers to save.")).toBeNull();
});
it("blocks saving a merged draft until the merge's markers are gone, then saves new heading underlines", async () => {
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  const store = libraryStore(app.profile.profileId);
  store.saveDraft({ base: { ...response.file, text: "Title\n=======\nA\n" }, phase: "draft", updatedAt: 1, reviewMerge: true, mergeMarkers: true,
    text: "Title\n=======\n<<<<<<< Draft on this device\nB\n=======\nC\n>>>>>>> Now on the computer\n" });
  render(<File route={route} />, { wrapper });
  await advance(1000);
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  const review = () => editor.getByRole("button", { name: "Review changes" });
  const type = (value: string) => fireEvent.change(editor.getByLabelText("Markdown source"), { target: { value } });
  expect(review()).toHaveProperty("disabled", true);
  expect(editor.getByText("Remove the conflict markers to save.")).toBeTruthy();
  expect(editor.getByText(/Overlaps you chose to edit by hand/)).toBeTruthy();
  type("Title\n=======\nB\n=======\nC\n");
  expect(review()).toHaveProperty("disabled", true);
  expect(editor.getByText("Remove the conflict markers to save.")).toBeTruthy();
  type("Title\n=======\nB\n");
  expect(review()).toHaveProperty("disabled", false);
  expect(editor.queryByText("Remove the conflict markers to save.")).toBeNull();
  expect(store.draft(route.rootId, route.path)?.mergeMarkers).toBe(false);
  type("Title\n=======\nB\n\nLicense\n=======\nApache-2.0\n");
  expect(review()).toHaveProperty("disabled", false);
});
