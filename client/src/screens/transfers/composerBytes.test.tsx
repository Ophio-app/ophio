// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { uuidv7 } from "../../protocol/ids";
import { RouteProvider } from "../../shell/Screen";
import { blobs } from "../../state/annotations";
import type { Route } from "../../app/ui";
import Inbox from "../work/Inbox";
import { inboxStore } from "../work/inbox";
import { chooseComposerFile, composerFileKey, TransferComposer, transferComposer } from "./composer";
import { TransferManager, transferManager, type TransferJob } from "./manager";
import { transferBytes } from "./storage";
import Transfers from "./Transfers";

vi.mock("./storage", () => {
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
const inTab = (tab: "library" | "work", route: Route = { screen: "transfers" }) => ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab, route, depth: 1, active: tab === "library" }}>{children}</RouteProvider>
  </AppProvider>
);
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("Send uploads the bytes of the file the composer names, even when another view chose it", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  // Transfers is open in the Library tab, and a file was chosen there.
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  const input = container.querySelector<HTMLInputElement>("input[type=file]")!;
  await act(async () => {
    fireEvent.change(input, { target: { files: [new File(["quarterly report"], "report.pdf")] } });
  });
  await advance();
  // From the Work tab, the capture inbox puts a shared photo in the composer
  // (Inbox.sendToComputer) and opens Transfers there.
  await fromElsewhere("holiday photo bytes", "photo.jpg");
  // Back in the Library tab, the composer now names photo.jpg. Send it.
  expect(screen.getAllByText("photo.jpg").length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  const [name, blob] = startUpload.mock.calls[0]!;
  expect(name).toBe("photo.jpg");
  expect(await blob.text()).toBe("holiday photo bytes");
});

/** What the capture inbox does to put a file in the composer (Inbox.sendToComputer). */
async function fromElsewhere(text: string, fileName = "notes.txt") {
  const fileId = uuidv7(), blob = new Blob([text]);
  await act(async () => {
    await transferBytes.put(composerFileKey(app.profile.profileId, fileId), blob);
    chooseComposerFile(app.profile.profileId, { fileId, fileName, fileSize: blob.size });
  });
  await advance();
  return fileId;
}

const chooseFile = async (container: HTMLElement) => {
  const input = container.querySelector<HTMLInputElement>("input[type=file]")!;
  await act(async () => {
    fireEvent.change(input, { target: { files: [new File(["notes"], "notes.txt")] } });
  });
  await advance();
};

it.each(["choice", "retry"])("keeps equal-metadata file bytes when an unread composer recovers during %s", async (recovery) => {
  const profileId = `unread-${recovery}`;
  const earlier = { fileName: "notes.txt", fileSize: 3, fileId: "earlier" };
  new TransferComposer(profileId).chooseFile(earlier);
  await transferBytes.put(composerFileKey(profileId, "earlier"), new Blob(["old"]));
  await transferBytes.put(composerFileKey(profileId, "new"), new Blob(["new"]));
  const read = vi.spyOn(Storage.prototype, "getItem");
  const fail = () => { throw new Error("Storage temporarily unavailable"); };
  if (recovery === "choice") read.mockImplementationOnce(fail);
  else read.mockImplementation(fail);
  chooseComposerFile(profileId, { ...earlier, fileId: "new" });
  read.mockRestore();
  expect(transferComposer(profileId).persistNow()).toBe(true);
  expect(new TransferComposer(profileId).get().fileId).toBe("new");
  expect(JSON.parse(localStorage.getItem(`transfer-composer.${profileId}.recovered`)!)).toMatchObject(earlier);
  expect(await (await transferBytes.get(composerFileKey(profileId, "earlier")))?.text()).toBe("old");
});

it("keeps the file still named on disk when choosing another file cannot be saved", async () => {
  const profileId = "file-write-failure";
  transferComposer(profileId).chooseFile({ fileName: "notes.txt", fileSize: 3, fileId: "earlier" });
  await transferBytes.put(composerFileKey(profileId, "earlier"), new Blob(["old"]));
  await transferBytes.put(composerFileKey(profileId, "new"), new Blob(["new"]));
  const write = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Full"); });
  chooseComposerFile(profileId, { fileName: "notes.txt", fileSize: 3, fileId: "new" });
  write.mockRestore();
  expect(new TransferComposer(profileId).get().fileId).toBe("earlier");
  expect(await (await transferBytes.get(composerFileKey(profileId, "earlier")))?.text()).toBe("old");
});

it("a computer without a received-files folder can't be chosen, and Send says why beside it", async () => {
  const sim = window.__sim!.host;
  const { inbox_path: _, ...host } = sim.host;
  sim.host = host;
  sim.emit({ type: "host_updated", host });
  await advance();
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  await chooseFile(container);
  const inbox = screen.getByRole("radio", { name: "Inbox on studio · not set up" }) as HTMLInputElement;
  expect(inbox.disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText("studio has no folder for received files. Choose another destination.")).toBeTruthy();
  fireEvent.click(screen.getByRole("radio", { name: "A project folder" }));
  await advance();
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(false);
});

it("a Send the computer refuses says so beside Send, not only at the top of the page", async () => {
  const query = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "upload_destination"
    ? Promise.reject(new Error("Received-files folder is not configured"))
    : query(q));
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  await chooseFile(container);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(1000);
  expect(startUpload).not.toHaveBeenCalled();
  const alert = container.querySelector(".transfer-footer [role=alert]");
  expect(alert?.textContent).toMatch(/^Not sent\. /);
});

it("a late answer about the destination the person has since left is not shown or acted on", async () => {
  const query = app.session.query.bind(app.session);
  let refuse: (e: Error) => void = () => undefined;
  const checks: string[] = [];
  vi.spyOn(app.session, "query").mockImplementation((q) => {
    if (q.kind !== "upload_destination") return query(q);
    checks.push(q.destination.kind);
    return q.destination.kind === "inbox"
      ? new Promise((_, reject) => { refuse = reject; })
      : query(q);
  });
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  await chooseFile(container);
  act(() => transferComposer(app.profile.profileId).update({ destination: { kind: "inbox" } }));
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  fireEvent.click(screen.getByRole("radio", { name: "A project folder" }));
  await advance();
  const sendButton = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
  expect(sendButton.disabled).toBe(false);
  await act(async () => { refuse(new Error("Received-files folder is not configured")); });
  await advance();
  expect(container.querySelector(".transfer-footer [role=alert]")).toBeNull();
  expect(startUpload).not.toHaveBeenCalled();
  fireEvent.click(sendButton);
  await advance(1000);
  expect(checks).toEqual(["inbox", "project_dir"]);
  expect(startUpload).toHaveBeenCalledTimes(1);
  expect(startUpload.mock.calls[0]?.[2]).toMatchObject({ kind: "project_dir" });
});

it("choosing the destination already chosen keeps its pending check and shows its refusal", async () => {
  const query = app.session.query.bind(app.session);
  let refuse: (e: Error) => void = () => undefined;
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "upload_destination"
    ? new Promise((_, reject) => { refuse = reject; })
    : query(q));
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  await chooseFile(container);
  fireEvent.click(screen.getByRole("radio", { name: "A project folder" }));
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  // The same folder again, as "Use this folder" without moving does.
  const composer = transferComposer(app.profile.profileId);
  act(() => composer.update({ destination: { ...composer.get().destination } }));
  await advance();
  await act(async () => { refuse(new Error("That folder is not writable")); });
  await advance();
  expect(container.querySelector(".transfer-footer [role=alert]")?.textContent).toMatch(/^Not sent\. /);
  expect(startUpload).not.toHaveBeenCalled();
});

const destinationChecks = () => {
  const query = app.session.query.bind(app.session);
  const answers: ((exists: boolean) => void)[] = [];
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "upload_destination"
    ? new Promise((resolve) => {
        answers.push((exists) => resolve({ check: { exists, keep_both_name: "note (2).md", replace_revision: null, replace_unavailable: null } } as never));
      })
    : query(q));
  return answers;
};

it("a check left pending when Transfers closes sends nothing, even once Transfers is open again", async () => {
  const answers = destinationChecks();
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = transferComposer(app.profile.profileId);
  // The composer outlives each test here, as it outlives each view.
  act(() => composer.update({ mode: "text", text: "first draft", name: "note.md", destination: { kind: "inbox" }, submitted: undefined }));
  const first = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  first.unmount();
  // Back, then Transfers again: a new folder and new text.
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  fireEvent.click(screen.getByRole("radio", { name: "A project folder" }));
  act(() => composer.update({ text: "second draft" }));
  await advance();
  await act(async () => { answers[0]!(false); });
  await advance();
  expect(startUpload).not.toHaveBeenCalled();
  expect(composer.get()).toMatchObject({ text: "second draft", submitted: undefined });
  // Send in the open view checks and sends what it shows.
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  await act(async () => { answers[1]!(false); });
  await advance();
  expect(startUpload).toHaveBeenCalledOnce();
  const [, blob, destination] = startUpload.mock.calls[0]!;
  expect(await blob.text()).toBe("second draft");
  expect(destination).toMatchObject({ kind: "project_dir" });
});

it("text changed while its check is pending is not sent as the old text, or cleared when that upload finishes", async () => {
  const answers = destinationChecks();
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = transferComposer(app.profile.profileId);
  // The composer outlives each test here, as it outlives each view.
  act(() => composer.update({ mode: "text", text: "first draft", name: "note.md", destination: { kind: "inbox" }, submitted: undefined }));
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  act(() => composer.update({ text: "second draft" }));
  await advance();
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(false);
  await act(async () => { answers[0]!(false); });
  await advance();
  expect(startUpload).not.toHaveBeenCalled();
  expect(composer.get()).toMatchObject({ text: "second draft", submitted: undefined });
  // The same text again is not a change: its check still sends.
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  act(() => composer.update({ text: "second draft" }));
  await act(async () => { answers[1]!(false); });
  await advance();
  expect(startUpload).toHaveBeenCalledOnce();
  expect(await startUpload.mock.calls[0]![1].text()).toBe("second draft");
});

it("a name already taken is no longer asked about once the composer names another file", async () => {
  const answers = destinationChecks();
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = transferComposer(app.profile.profileId);
  act(() => composer.update({ mode: "text", text: "first draft", name: "note.md", destination: { kind: "inbox" }, submitted: undefined }));
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(100);
  await act(async () => { answers[0]!(true); });
  await advance();
  expect(container.querySelector("dialog[open]")?.textContent).toContain("note.md already exists");
  // Another view renames what the composer will send.
  act(() => composer.update({ name: "plan.md" }));
  await advance();
  expect(container.querySelector("dialog[open]")).toBeNull();
  expect(startUpload).not.toHaveBeenCalled();
});

it("a draft saved for Attach to a message keeps its text and waits for the Inbox or a project folder", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = transferComposer(app.profile.profileId);
  // Saved before Transfers stopped offering uploads for messages.
  act(() => composer.update({ mode: "text", text: "Notes for the agent.", name: "note.md", destination: { kind: "artifact" }, submitted: undefined }));
  const { container } = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  expect(screen.queryByRole("radio", { name: "Attach to a message" })).toBeNull();
  const inbox = screen.getByRole("radio", { name: "/home/sam/Downloads/ophio on studio" }) as HTMLInputElement;
  const folder = screen.getByRole("radio", { name: "A project folder" }) as HTMLInputElement;
  expect([inbox.checked, folder.checked]).toEqual([false, false]);
  expect((screen.getByLabelText("File text") as HTMLTextAreaElement).value).toBe("Notes for the agent.");
  expect(screen.getByText(/Files sent from here can’t be attached to a message/)).toBeTruthy();
  expect(container.querySelector(".transfer-footer")?.textContent).toContain("Choose where to send this file.");
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(inbox);
  await advance();
  expect(container.querySelector(".transfer-footer")?.textContent).not.toContain("Choose where to send this file.");
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  const [name, blob, destination] = startUpload.mock.calls[0]!;
  expect(name).toBe("note.md");
  expect(await blob.text()).toBe("Notes for the agent.");
  expect(destination).toEqual({ kind: "inbox" });
});

/** A file draft with nothing chosen; the composer outlives each test here. */
const emptyFileDraft = () => {
  const composer = transferComposer(app.profile.profileId);
  act(() => composer.update({ mode: "file", fileName: "", fileSize: 0, fileId: undefined, destination: { kind: "inbox" }, submitted: undefined }));
  return composer;
};
const pickIn = async (container: HTMLElement, file: File) => {
  await act(async () => {
    fireEvent.change(container.querySelector<HTMLInputElement>("input[type=file]")!, { target: { files: [file] } });
  });
  await advance();
};
/** Storage reads that answer only when released, with what was kept when they began. */
const slowReads = () => {
  const get = transferBytes.get, reads: { key: string; release: () => Promise<void> }[] = [];
  vi.spyOn(transferBytes, "get").mockImplementation((key) => {
    const kept = get(key);
    return new Promise((resolve) => {
      reads.push({ key, release: () => act(async () => { resolve(await kept); }) });
    });
  });
  return reads;
};
const sendButton = (container: HTMLElement) => within(container).getByRole("button", { name: "Send" }) as HTMLButtonElement;

it("Send uploads a replacement from the capture inbox with the same name and size, not the file it replaced", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = emptyFileDraft();
  // In Library, Transfers has notes.txt saying "old".
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  await pickIn(library.container, new File(["old"], "notes.txt"));
  const first = composer.get().fileId;
  // From the capture inbox in Tasks, another notes.txt of the same size, saying "new".
  vi.spyOn(blobs, "get").mockResolvedValue(new Blob(["new"]));
  act(() => { inboxStore(app.profile.profileId).add({ kind: "file", blobKey: "shared-notes", name: "notes.txt", mediaType: "text/plain", size: 3, text: null, sourceApp: null, projectId: null }); });
  const tasks = render(<Inbox route={{ screen: "inbox" }} />, { wrapper: inTab("work", { screen: "inbox" }) });
  await advance();
  fireEvent.click(within(tasks.container).getByRole("button", { name: "Send to computer…" }));
  await advance();
  // Comparing equal-sized files uses the asynchronous SHA-256 implementation.
  await vi.waitFor(() => expect(within(tasks.container).getByRole("button", { name: "Replace notes.txt" })).toBeTruthy());
  expect(composer.get().fileId).toBe(first);
  expect(await (await transferBytes.get(composerFileKey(app.profile.profileId, first)))?.text()).toBe("old");
  fireEvent.click(within(tasks.container).getByRole("button", { name: "Replace notes.txt" }));
  await advance();
  await vi.waitFor(() => expect(composer.get().fileId).not.toBe(first));
  expect(composer.get()).toMatchObject({ fileName: "notes.txt", fileSize: 3 });
  // The replaced bytes are let go.
  expect(await transferBytes.get(composerFileKey(app.profile.profileId, first))).toBeNull();
  // Back in Library, the name and size Send shows are unchanged. Send.
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  // Reading the replacement's bytes is real storage work, slower when the whole suite runs.
  await vi.waitFor(() => expect(startUpload).toHaveBeenCalledOnce());
  const [name, blob] = startUpload.mock.calls[0]!;
  expect(name).toBe("notes.txt");
  expect(await blob.text()).toBe("new");
});

it("Send waits for the bytes of the file chosen, and a slow read of an earlier one is never sent in its place", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  emptyFileDraft();
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  const tasks = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("work") });
  await advance();
  const reads = slowReads();
  // Transfers in Tasks chooses notes.txt, then another notes.txt of the same size.
  await pickIn(tasks.container, new File(["one"], "notes.txt"));
  expect(reads).toHaveLength(1);
  // Library names it, but its bytes are still being read: Send waits, and
  // nothing says the file must be chosen again.
  expect(within(library.container).getAllByText("notes.txt").length).toBeGreaterThan(0);
  expect(sendButton(library.container).disabled).toBe(true);
  expect(within(library.container).queryByText(/Select the original file again/)).toBeNull();
  await pickIn(tasks.container, new File(["two"], "notes.txt"));
  expect(reads).toHaveLength(2);
  // The read of the first choice answers late, with its bytes.
  await reads[0]!.release();
  await advance();
  expect(sendButton(library.container).disabled).toBe(true);
  await reads[1]!.release();
  await advance();
  expect(sendButton(library.container).disabled).toBe(false);
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  expect(await startUpload.mock.calls[0]![1].text()).toBe("two");
});

it("Send again replaces equal-metadata bytes across views and waits for the replacement read", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("retry-job");
  const composer = emptyFileDraft();
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  const tasks = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("work") });
  await advance();
  await pickIn(library.container, new File(["old"], "notes.txt"));
  const first = composer.get().fileId;
  const manager = transferManager(app), now = Date.now();
  const failed: TransferJob = { id: "failed-upload", name: "notes.txt", direction: "upload", size: 3, transferred: 0, phase: "failed", createdAt: now, updatedAt: now, transferId: null, sha256: null, destination: { kind: "inbox" }, source: null, artifactId: null, hostPath: null, error: "Interrupted", storageError: null, resumable: false };
  act(() => manager.set((m) => ({ ...m, jobs: { ...m.jobs, [failed.id]: failed } })));
  vi.spyOn(manager, "blob").mockResolvedValue(new Blob(["new"]));
  const reads = slowReads();
  fireEvent.click(within(tasks.container).getByRole("button", { name: "Send again…" }));
  await advance();
  expect(composer.get()).toMatchObject({ fileName: "notes.txt", fileSize: 3 });
  expect(composer.get().fileId).not.toBe(first);
  expect(startUpload).not.toHaveBeenCalled();
  expect(reads).toHaveLength(1);
  expect(sendButton(library.container).disabled).toBe(true);
  await reads[0]!.release();
  await advance();
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  expect(await startUpload.mock.calls[0]![1].text()).toBe("new");
});

it("a file this device couldn't keep is still sent from the view that chose it, not replaced by a late read", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  emptyFileDraft();
  await fromElsewhere("aaa");
  const reads = slowReads();
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  expect(reads).toHaveLength(1);
  let refuse: (e: Error) => void = () => undefined;
  vi.spyOn(transferBytes, "put").mockImplementationOnce(() => new Promise((_, reject) => { refuse = reject; }));
  await pickIn(library.container, new File(["ppp"], "notes.txt"));
  // The earlier file's bytes arrive while the new file is being kept.
  await reads[0]!.release();
  await advance();
  await act(async () => { refuse(new Error("This device’s file storage is full or unavailable.")); });
  await advance();
  expect(within(library.container).getByText(/Keep the app open until you send this file/)).toBeTruthy();
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  expect(await startUpload.mock.calls[0]![1].text()).toBe("ppp");
});

it("a file chosen before choices had their own identity is still sent", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = emptyFileDraft();
  await act(async () => {
    await transferBytes.put(composerFileKey(app.profile.profileId), new Blob(["kept before"]));
    composer.update({ fileName: "notes.txt", fileSize: 11 });
  });
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(await startUpload.mock.calls[0]![1].text()).toBe("kept before");
});

it("an earlier upload finishing doesn't let go of a file still being chosen", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job-a");
  emptyFileDraft();
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  await pickIn(library.container, new File(["aaa"], "notes.txt"));
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  // Another file is chosen while the first uploads, and this device can't keep it.
  let refuse: (e: Error) => void = () => undefined;
  vi.spyOn(transferBytes, "put").mockImplementationOnce(() => new Promise((_, reject) => { refuse = reject; }));
  await pickIn(library.container, new File(["bbb"], "notes.txt"));
  const manager = transferManager(app), now = Date.now();
  const sent: TransferJob = { id: "job-a", name: "notes.txt", direction: "upload", size: 3, transferred: 3, phase: "completed", createdAt: now, updatedAt: now, transferId: null, sha256: null, destination: { kind: "inbox" }, source: null, artifactId: null, hostPath: null, error: null, storageError: null, resumable: false };
  act(() => manager.set((m) => ({ ...m, jobs: { ...m.jobs, "job-a": sent } })));
  await advance();
  await act(async () => { refuse(new Error("This device’s file storage is full or unavailable.")); });
  await advance();
  startUpload.mockReturnValue("job-b");
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledTimes(2);
  expect(await startUpload.mock.calls[1]![1].text()).toBe("bbb");
});

it("Text chosen while a file is being kept stays what Send sends", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("job");
  const composer = emptyFileDraft();
  act(() => composer.update({ text: "", name: "note.md" }));
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  await advance();
  let keep: () => void = () => undefined;
  vi.spyOn(transferBytes, "put").mockImplementationOnce(() => new Promise((resolve) => { keep = () => resolve(undefined); }));
  await pickIn(library.container, new File(["file bytes"], "notes.txt"));
  fireEvent.click(within(library.container).getByRole("radio", { name: "Text" }));
  fireEvent.change(within(library.container).getByLabelText("File text"), { target: { value: "typed meanwhile" } });
  await advance();
  await act(async () => { keep(); });
  await advance();
  // The file is kept for File, and Text stays the source.
  expect(composer.get()).toMatchObject({ mode: "text", text: "typed meanwhile", fileName: "notes.txt" });
  fireEvent.click(sendButton(library.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  const [name, blob] = startUpload.mock.calls[0]!;
  expect(name).toBe("note.md");
  expect(await blob.text()).toBe("typed meanwhile");
});

it("Text sent from another view clears after a pending file save finishes, while the file stays available", async () => {
  const startUpload = vi.spyOn(TransferManager.prototype, "startUpload").mockReturnValue("text-job");
  const composer = emptyFileDraft();
  act(() => composer.update({ text: "", name: "note.md" }));
  const library = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("library") });
  const tasks = render(<Transfers route={{ screen: "transfers" }} />, { wrapper: inTab("work") });
  await advance();
  const put = transferBytes.put;
  let keep = async () => {};
  vi.spyOn(transferBytes, "put").mockImplementationOnce((key, blob) => new Promise<undefined>((resolve) => {
    keep = async () => { await put(key, blob); resolve(undefined); };
  }));
  await pickIn(library.container, new File(["file bytes"], "notes.txt"));
  fireEvent.click(within(tasks.container).getByRole("radio", { name: "Text" }));
  fireEvent.change(within(tasks.container).getByLabelText("File text"), { target: { value: "typed meanwhile" } });
  await advance();
  // This view can send the shared text while Library still saves its file.
  expect(sendButton(tasks.container).disabled).toBe(false);
  fireEvent.click(sendButton(tasks.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledOnce();
  expect(await startUpload.mock.calls[0]![1].text()).toBe("typed meanwhile");
  await act(keep);
  await advance();
  const fileId = composer.get().fileId;
  expect(fileId).toBeTruthy();
  const manager = transferManager(app), now = Date.now();
  const sent: TransferJob = { id: "text-job", name: "note.md", direction: "upload", size: 15, transferred: 15, phase: "completed", createdAt: now, updatedAt: now, transferId: null, sha256: null, destination: { kind: "inbox" }, source: null, artifactId: null, hostPath: null, error: null, storageError: null, resumable: false };
  act(() => manager.set((m) => ({ ...m, jobs: { ...m.jobs, [sent.id]: sent } })));
  await advance();
  expect(composer.get()).toMatchObject({ mode: "text", text: "", submitted: undefined, fileName: "notes.txt", fileId });
  const reopened = new TransferComposer(app.profile.profileId).get();
  expect(reopened).toMatchObject({ text: "", fileName: "notes.txt", fileId });
  expect(reopened.submitted).toBeUndefined();
  expect(within(tasks.container).queryByRole("button", { name: "Send" })).toBeNull();
  expect(await (await transferBytes.get(composerFileKey(app.profile.profileId, fileId)))?.text()).toBe("file bytes");
  fireEvent.click(within(tasks.container).getByRole("radio", { name: "File" }));
  await advance();
  startUpload.mockReturnValue("file-job");
  fireEvent.click(sendButton(tasks.container));
  await advance(1000);
  expect(startUpload).toHaveBeenCalledTimes(2);
  expect(await startUpload.mock.calls[1]![1].text()).toBe("file bytes");
});
