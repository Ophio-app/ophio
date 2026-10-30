// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Tab } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Toasts } from "../../design/Toast";
import { UrgentSlot } from "../../design/urgent";
import SafetyStrip from "../control/SafetyStrip";
import { libraryStore } from "./drafts";
import Conflict from "./Conflict";
import File from "./File";
import Folder from "./Folder";
import { MovedDrafts } from "./MovedDrafts";
import { RecoveryList } from "./RecoveryList";
import { Diff } from "./shared";
import { saveDraft } from "./save";
vi.mock("./Editor", () => ({ default: ({ initialText, onChange }: { initialText: string; onChange?(text: string): void }) => <textarea aria-label="Markdown source" defaultValue={initialText} onChange={(e) => onChange?.(e.target.value)} /> }));
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
    profile: { ...profile, profileId: `completion-${serial++}` },
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
it("explains revoked writes beside Review and restores the action without losing text", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  // Nothing is kept, or reviewable, before a change.
  expect(editor.getByText("No changes yet")).toBeTruthy();
  expect(editor.queryByText(/^Saved on this device/)).toBeNull();
  expect(editor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", true);
  fireEvent.change(editor.getByLabelText("Markdown source"), { target: { value: "# Windows notes\n\nKeep the line endings.\nAnd the tabs.\n" } });
  await advance();
  expect(editor.getByText(/^Saved on this device/)).toBeTruthy();
  expect(editor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", false);
  window.__sim?.permissions({ library_write: false, admin: false });
  await advance(3000);
  expect(editor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", true);
  expect(editor.getByText(`ophio devices allow ${app.profile.deviceId} library-write`)).toBeTruthy();
  expect(editor.getByLabelText("Markdown source")).toHaveProperty("value", "# Windows notes\n\nKeep the line endings.\nAnd the tabs.\n");
  window.__sim?.permissions({ library_write: true });
  await advance(3000);
  expect(editor.getByRole("button", { name: "Review changes" })).toHaveProperty("disabled", false);
  expect(editor.queryByText("This device may not save Library changes")).toBeNull();
});
it("discards local text after an uncertain save without discarding its unresolved status", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const store = libraryStore(app.profile.profileId), draft = store.draft(route.rootId, route.path);
  if (!draft) throw new Error("Missing draft");
  host().library.uncertainNextSave();
  expect((await run(() => saveDraft(app.session, store, { ...draft, text: "Saved despite uncertainty\n" }))).kind).toBe("unknown");
  fireEvent.click(screen.getByRole("button", { name: "Close editor" }));
  const confirm = within(screen.getByRole("dialog", { name: "Keep your device draft?" }));
  expect(confirm.getByText(/earlier save may already have changed/)).toBeTruthy();
  expect(confirm.queryByText(/file stays unchanged/)).toBeNull();
  fireEvent.click(confirm.getByRole("button", { name: "Discard draft for windows.md" }));
  expect(store.draft(route.rootId, route.path)).toBeNull();
  expect(screen.getByText("The earlier save still needs checking")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Check computer file" }));
  await advance(1000);
  expect(screen.queryByText("The earlier save still needs checking")).toBeNull();
  expect(screen.getByText(/Showing the file just read/)).toBeTruthy();
});

it.each([false, true])("defers a change notice during an uncertain save, foreign change: %s", async (foreign) => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const store = libraryStore(app.profile.profileId), draft = store.draft(route.rootId, route.path)!;
  host().library.uncertainNextSave();
  await run(() => saveDraft(app.session, store, { ...draft, text: "My saved text\n" }));
  if (foreign) await act(async () => {
    await host().library.editOnComputer(route.rootId, route.path, "Someone else changed it\n");
  });
  expect(screen.queryByText("Changed on the computer since you opened it")).toBeNull();
  const editor = within(screen.getByRole("dialog", { name: "Edit Markdown" }));
  expect(editor.getByText("The computer hasn’t confirmed this save")).toBeTruthy();
  fireEvent.click(editor.getByRole("button", { name: "Check again" }));
  await advance(1000);
  if (foreign) {
    expect(store.draft(route.rootId, route.path)).toMatchObject({ phase: "conflict", current: { text: "Someone else changed it\n" } });
    expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toMatchObject({ screen: "conflict" });
  } else {
    expect(store.draft(route.rootId, route.path)).toBeNull();
    expect(screen.getByText("Saved on computer")).toBeTruthy();
    expect(screen.queryByText("Changed on the computer since you opened it")).toBeNull();
  }
});

it("retains a foreign revision received before our successful save receipt", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /^Edit$/ }));
  await advance();
  const store = libraryStore(app.profile.profileId), draft = store.draft(route.rootId, route.path)!;
  let confirm!: (value: Awaited<ReturnType<HostSession["command"]>>) => void;
  vi.spyOn(app.session, "command").mockImplementationOnce(() => new Promise(resolve => { confirm = resolve; }));
  let saving!: ReturnType<typeof saveDraft>;
  act(() => { saving = saveDraft(app.session, store, draft); });
  await act(async () => { await host().library.editOnComputer(route.rootId, route.path, "Foreign revision\n"); });
  expect(screen.queryByText("Changed on the computer since you opened it")).toBeNull();
  await act(async () => {
    confirm({ command_id: "save", seq: 1, status: "accepted", error: null,
      result: { kind: "saved", outcome: { outcome: "saved", revision: "my-save-revision" } } });
    await saving;
  });
  expect(screen.getAllByText("Changed on the computer since you opened it").length).toBeGreaterThan(0);
});

it("moves the device draft only after the file move is confirmed", async () => {
  const store = libraryStore(app.profile.profileId);
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  store.saveDraft({ base: response.file, text: "Local edit", phase: "draft", updatedAt: 1 });
  store.remember(route.rootId, route.path);
  render(<Folder route={{ screen: "folder", rootId: route.rootId, path: "docs" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Actions for windows.md" }));
  fireEvent.click(screen.getByRole("button", { name: "Move…" }));
  await advance(500);
  const move = within(screen.getByRole("dialog", { name: "Move file" }));
  fireEvent.click(move.getByRole("button", { name: "guides" }));
  await advance(500);
  fireEvent.click(move.getByRole("button", { name: "Choose this folder" }));
  fireEvent.click(move.getByRole("button", { name: "Move file" }));
  expect(store.draft(route.rootId, route.path)?.text).toBe("Local edit");
  await advance(1000);
  expect(store.draft(route.rootId, route.path)).toBeNull();
  expect(store.draft(route.rootId, "docs/guides/windows.md")?.text).toBe("Local edit");
  expect(store.get().recent).toEqual([]);
});

it("a deleted file can be taken back from the toast that says it went", async () => {
  render(<><Folder route={{ screen: "folder", rootId: route.rootId, path: "docs" }} /><Toasts /></>, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Actions for windows.md" }));
  fireEvent.click(screen.getByRole("button", { name: /^Delete/ }));
  await advance(500);
  fireEvent.click(within(screen.getByRole("dialog", { name: "Delete file?" })).getByRole("button", { name: "Delete windows.md" }));
  await advance(1000);
  expect(host().files[route.rootId]?.[route.path]).toBeUndefined();
  expect(screen.getByText("Deleted windows.md")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  await advance(1000);
  expect(host().files[route.rootId]?.[route.path]).toBeDefined();
  expect(screen.getByText("Restored windows.md")).toBeTruthy();
});

it("offers Open and explicit Discard when a destination already has a draft", async () => {
  const store = libraryStore(app.profile.profileId);
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  const base = response.file, destination = "docs/guides/windows.md";
  store.saveDraft({ base, text: "Old text", phase: "draft", updatedAt: 1 });
  store.saveDraft({ base: { ...base, path: destination }, text: "Destination text", phase: "draft", updatedAt: 2 });
  store.moveFile(route.rootId, route.path, destination);
  render(<MovedDrafts rootId={route.rootId} path={destination} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Open old draft" }));
  expect(screen.getByText("Old text")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Close$/ }));
  fireEvent.click(screen.getByRole("button", { name: "Discard old draft…" }));
  fireEvent.click(screen.getByRole("button", { name: "Discard old device draft" }));
  expect(store.draft(route.rootId, route.path)).toBeNull();
  expect(store.draft(route.rootId, destination)?.text).toBe("Destination text");
});

it("cannot re-open a moved recovery draft for saving at its old path", async () => {
  const store = libraryStore(app.profile.profileId);
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  store.saveDraft({ base: response.file, text: "Kept text", phase: "draft", updatedAt: 1,
    movedTo: "docs/guides/windows.md", reviewMerge: true });
  render(<File route={route} />, { wrapper });
  await advance(1000);
  expect(screen.queryByRole("dialog", { name: "Edit Markdown" })).toBeNull();
  expect(screen.getByRole("button", { name: "Open draft" })).toHaveProperty("disabled", true);
  expect(store.get().recent).toEqual([]);
});

it("uses a quiet heading when no recovery copy needs checking", async () => {
  vi.spyOn(app.session, "query").mockResolvedValue({ kind: "library_recovery_list", items: [] });
  render(<RecoveryList />, { wrapper }); await advance();
  expect(screen.getByText("Recovery copies · none")).toBeTruthy();
  expect(screen.queryByText("Needs checking")).toBeNull();
});
it.each([false, true])("shows overlap advice only for merged drafts with markers (%s)", async markers => {
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  libraryStore(app.profile.profileId).saveDraft({ base: response.file, text: markers ? "<<<<<<< Draft on this device\nMine\n=======\nTheirs\n>>>>>>> Computer" : "Merged without overlaps", phase: "draft", updatedAt: 1, reviewMerge: true });
  render(<File route={route} />, { wrapper }); await advance(1000);
  expect(screen.getByText("Merged draft · review every change")).toBeTruthy();
  expect(!!screen.queryByText(/Overlaps you chose to edit by hand are between markers/)).toBe(markers);
});
it.each([["a\n\nb", "a\nb", "remove", "−", "Removed"], ["a\nb", "a\n\nb", "add", "+", "Added"]])("renders blank diff lines with a single sign", (base, text, kind, sign, spoken) => {
  const { container } = render(<Diff base={base!} text={text!} />);
  const line = container.querySelector(`.diff__line--${kind}`)!;
  expect(line.children.length).toBe(2);
  expect(line.firstElementChild?.className).toBe("diff__sign");
  expect(line.firstElementChild?.textContent).toBe(sign);
  const code = line.querySelector("code")!;
  expect(code.textContent).toBe(`${spoken}: blank line`);
  expect([...code.children].every((c) => c.className === "visually-hidden")).toBe(true);
});

it("lists a displaced write with its reason", async () => {
  const { file } = await run(() => app.session.query({ kind: "library_read", root_id: route.rootId, path: route.path }));
  const item = { root_id: route.rootId, path: route.path, recovery_id: "late-writer", created_at: Date.now() };
  vi.spyOn(app.session, "query").mockImplementation(async query => {
    if (query.kind === "library_recovery_list") return { kind: "library_recovery_list", items: [item] };
    return { kind: "library_recovery_read", recovery: { item, observed_at: Date.now(),
      base: { state: "text", file }, proposed: { state: "text", file }, current: { state: "text", file },
      displaced: { state: "text", file: { ...file, revision: "late-write", text: "Late native words" } } } };
  });
  render(<RecoveryList />, { wrapper });
  await advance();
  expect(screen.getByText("Needs checking")).toBeTruthy();
  expect(screen.getByText(/The file changed on the computer while it was being saved/)).toBeTruthy();
});

it("hides reserved backups even with Show hidden files", async () => {
  const { listing } = await run(() => app.session.query({ kind: "library_list", root_id: route.rootId, dir: "docs" }));
  vi.spyOn(app.session, "query").mockResolvedValue({ kind: "library_list", listing: { ...listing, entries: [
    ...listing.entries,
    { ...listing.entries[0]!, name: ".ophio-save-test-old", path: "docs/.ophio-save-test-old", hidden: true },
    { ...listing.entries[0]!, name: ".companion-save-test-old", path: "docs/.companion-save-test-old", hidden: true },
    { ...listing.entries[0]!, name: ".personal.md", path: "docs/.personal.md", hidden: true },
  ] } });
  render(<Folder route={{ screen: "folder", rootId: route.rootId, path: "docs" }} />, { wrapper });
  await advance();
  fireEvent.click(screen.getByLabelText("Show hidden files"));
  expect(screen.getByText(".personal.md")).toBeTruthy();
  expect(screen.queryByText(".ophio-save-test-old")).toBeNull();
  expect(screen.queryByText(".companion-save-test-old")).toBeNull();
});

it("closes an unchanged editor without inventing an unsaved draft", async () => {
  render(<File route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Close editor" }));
  expect(screen.queryByRole("dialog", { name: "Keep your device draft?" })).toBeNull();
  expect(libraryStore(app.profile.profileId).draft(route.rootId, route.path)).toBeNull();
});

it("offers the conflict choices when the computer's version is too large to show", async () => {
  const store = libraryStore(app.profile.profileId);
  const response = await host().query({ kind: "library_read", root_id: route.rootId, path: route.path });
  if (response.kind !== "library_read") throw new Error("Missing file");
  store.saveDraft({ base: response.file, text: "Local edit\n", phase: "conflict", updatedAt: 1, current: null, currentTooLarge: true, currentAt: 2 });
  render(<Conflict route={{ screen: "conflict", rootId: route.rootId, path: route.path }} />, { wrapper });
  await advance();
  expect(screen.getByText("The computer’s version is too large to show here")).toBeTruthy();
  expect(screen.getByText("Too large to show on this device")).toBeTruthy();
  expect(screen.queryByText("File missing")).toBeNull();
  expect(screen.queryByRole("radio", { name: "Base → Computer" })).toBeNull();
  expect(screen.getByRole("button", { name: "Merge into a new draft" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "Save as a new copy…" })).toHaveProperty("disabled", false);
  expect(screen.getByRole("button", { name: "Keep draft for later" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Discard my draft…" })).toBeTruthy();
});

function openFolder(path: string, tab: Tab = "library") {
  const folder = { screen: "folder", rootId: route.rootId, path } as const;
  app.ui.push(folder, tab);
  render(<RouteProvider value={{ tab, route: folder, depth: 1, active: true }}>
    <UrgentSlot.Provider value={SafetyStrip}><Folder route={folder} /></UrgentSlot.Provider>
  </RouteProvider>, { wrapper });
  return folder;
}

it.each([["", "library"], ["docs", "library"], ["docs", "work"]] as const)("clears the typed name and opens a new file in folder '%s' on %s", async (path, tab) => {
  openFolder(path, tab);
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "New file" }));
  const sheet = within(screen.getByRole("dialog", { name: "New file" }));
  fireEvent.change(sheet.getByLabelText("File name"), { target: { value: "notes.md" } });
  fireEvent.click(sheet.getByRole("button", { name: "Create file" }));
  await advance(1000);
  expect(screen.getByText("Created notes.md")).toBeTruthy();
  const createdPath = path ? `${path}/notes.md` : "notes.md";
  expect(host().files[route.rootId]?.[createdPath]).toBe("");
  expect(app.ui.get().tab).toBe(tab);
  // It opens to be written in.
  expect(app.ui.route()).toEqual({ screen: "file", rootId: route.rootId, path: createdPath, edit: true });
  act(() => app.ui.back());
  fireEvent.click(screen.getByRole("button", { name: "New file" }));
  expect((within(screen.getByRole("dialog", { name: "New file" })).getByLabelText("File name") as HTMLInputElement).value).toBe("");
});

it.each(["duplicate", "lost receipt"])("keeps the typed name without navigating after %s", async (failure) => {
  openFolder("docs");
  const push = vi.spyOn(app.ui, "push");
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "New file" }));
  const sheet = within(screen.getByRole("dialog", { name: "New file" }));
  const name = failure === "duplicate" ? "windows.md" : "notes.md";
  fireEvent.change(sheet.getByLabelText("File name"), { target: { value: name } });
  if (failure === "lost receipt") window.__sim!.dropNextReceipt();
  fireEvent.click(sheet.getByRole("button", { name: "Create file" }));
  await advance(1000);
  expect(sheet.getByText(failure === "duplicate" ? /already exists/ : /computer hasn’t confirmed this change/)).toBeTruthy();
  expect(sheet.getByLabelText("File name")).toHaveProperty("value", name);
  expect(libraryStore(app.profile.profileId).get().forms[`name:${route.rootId}:docs:new`]).toBe(name);
  expect(push).not.toHaveBeenCalled();
});

it.each(["Open task", "Open control", "another folder", "reopened folder"])("keeps %s selected after a delayed file creation reply", async (destination) => {
  const folder = openFolder("docs");
  await advance(1000);
  let confirm!: (value: Awaited<ReturnType<HostSession["command"]>>) => void;
  const command = vi.spyOn(app.session, "command").mockImplementationOnce(() => new Promise(resolve => { confirm = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "New file" }));
  const sheet = within(screen.getByRole("dialog", { name: "New file" }));
  fireEvent.change(sheet.getByLabelText("File name"), { target: { value: "notes.md" } });
  fireEvent.click(sheet.getByRole("button", { name: "Create file" }));
  expect(command).toHaveBeenCalledWith({ command: "library_create", root_id: route.rootId, path: "docs/notes.md", text: "" }, expect.anything());
  if (destination === "Open task" || destination === "Open control") {
    const safety = within(sheet.getByRole("complementary", { name: "Safety controls" }));
    fireEvent.click(safety.getByRole("button", { name: /Safety/ }));
    fireEvent.click(safety.getAllByRole("button", { name: new RegExp(`^${destination}`) })[0]!);
    expect(app.ui.get().tab).toBe(destination === "Open task" ? "talk" : "control");
  } else {
    act(() => {
      if (destination === "reopened folder") app.ui.back();
      app.ui.push({ ...folder, path: destination === "reopened folder" ? "docs" : "guides" }, "library");
    });
  }
  const selected = app.ui.get();
  await act(async () => {
    confirm({ command_id: "create", seq: 1, status: "accepted", error: null,
      result: { kind: "created", path: "docs/notes.md", revision: "created-revision" } });
  });
  expect(app.ui.get()).toBe(selected);
  expect(libraryStore(app.profile.profileId).get().forms[`name:${route.rootId}:docs:new`]).toBe("");
});
