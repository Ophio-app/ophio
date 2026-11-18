// SPDX-License-Identifier: Apache-2.0
// Files on a chat message: chosen from the phone, shown as chips, uploaded
// before the message counts as sent, kept with the typed text when an upload
// fails, and sent again without uploading twice.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { TransferError, upload } from "../../host/transfer";
import { RouteProvider } from "../../shell/Screen";
import { blobs } from "../../state/annotations";
import { stopStore } from "../control/stops";
import { NEW_CONVERSATION } from "./conversation";
import { amount, fitting, MAX_FILE_BYTES, shownName, uploadName } from "./chatFiles";
import Talk from "./Talk";

vi.mock("../../host/transfer", async (original) => {
  const actual = await original<typeof import("../../host/transfer")>();
  return { ...actual, upload: vi.fn(actual.upload) };
});

describe("names and limits", () => {
  it("says sizes the way phones do", () => {
    expect([1, 999, 1000, 4_500_000, 82_000, 100_000_000, 1_000_000_000].map(amount))
      .toEqual(["1 byte", "999 bytes", "1 KB", "4.5 MB", "82 KB", "100 MB", "1 GB"]);
  });

  it("shows one plain line of the name, never a path or hidden characters", () => {
    expect(shownName("../../etc/passwd")).toBe("passwd");
    expect(shownName(String.raw`C:\Users\sam\notes.txt`)).toBe("notes.txt");
    expect(shownName("report\n\tfinal\u202Efdp.exe")).toBe("report final fdp.exe");
    expect(shownName("/")).toBe("file");
    expect([...shownName("a".repeat(400))].length).toBe(255);
  });

  it("gives the transfer a safe single name that keeps the extension", () => {
    expect(uploadName("my: report?.pdf")).toBe("my_ report_.pdf");
    expect(uploadName("CON.txt")).toBe("_CON.txt");
    expect(uploadName("...")).toBe("_file");
    const long = uploadName(`${"é".repeat(200)}.tar.gz`);
    expect(new TextEncoder().encode(long).length).toBeLessThanOrEqual(150);
    expect(long.endsWith(".gz")).toBe(true);
  });

  it("refuses what is too large or too many, in plain words with the limit", () => {
    const f = (name: string, size: number) => ({ name, size });
    expect(fitting([], [f("big.mov", 150_000_000), f("ok.txt", 10)])).toEqual({
      take: [f("ok.txt", 10)],
      refused: "“big.mov” is 150 MB. Files up to 100 MB can be attached.",
    });
    const nine = Array.from({ length: 9 }, () => f("x", 1));
    expect(fitting(nine, [f("a", 1), f("b", 1)]).refused).toBe("A message can carry up to 10 files. Send the rest in another message.");
    expect(fitting([f("x", 90_000_000), f("y", 90_000_000)], [f("z.zip", 90_000_000)]).refused)
      .toBe("“z.zip” would bring this message to 270 MB. One message can carry up to 250 MB; send it in another message.");
  });
});

describe("in a chat", () => {
  let app: App;
  const stored = new Map<string, Blob>();
  const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

  beforeEach(async () => {
    localStorage.clear(); vi.useFakeTimers(); stored.clear();
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
    HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    HTMLDialogElement.prototype.close = function () { this.open = false; };
    // Native digests don't settle under fake time; the phone and the simulator agree on this one.
    vi.spyOn(crypto.subtle, "digest").mockImplementation(async () => new Uint8Array(32).buffer);
    // This device's source storage, which jsdom lacks.
    let n = 0;
    vi.spyOn(blobs, "put").mockImplementation(async (b) => { const key = `blob-${++n}`; stored.set(key, b); return key; });
    vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
    vi.spyOn(blobs, "remove").mockImplementation(async (key) => { stored.delete(key); });
    const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
    const session = new HostSession(profile.profileId, platform.control(profile.profileId));
    app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
    session.start(); await tick(900);
    app.ui.selectProject(ids.pTide);
    await tick(100);
    const route = { screen: "talk" as const };
    render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
    await tick(100);
    fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
    await tick(100);
  });
  afterEach(() => {
    cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose();
    vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.mocked(upload).mockClear();
  });

  const file = (name: string, text = name, type = "text/plain") => new File([text], name, { type });
  const sized = (name: string, size: number) => { const f = file(name); Object.defineProperty(f, "size", { value: size }); return f; };
  const sheet = () => within(screen.getByRole("dialog", { name: "Attach" }));
  /** Attach is beside the words while they're empty, and under Message tools once there's something to send. */
  function openAttach() {
    const clip = screen.queryByRole("button", { name: "Attach" });
    if (clip) return fireEvent.click(clip);
    fireEvent.click(screen.getByRole("button", { name: "Message tools" }));
    fireEvent.click(within(screen.getByRole("dialog", { name: "Message tools" })).getByRole("button", { name: /^Attach/ }));
  }
  async function pick(...files: File[]) {
    openAttach();
    const input = screen.getByLabelText("Files to attach") as HTMLInputElement;
    expect(input.multiple).toBe(true);
    expect(input.accept).toBe("");
    Object.defineProperty(input, "files", { configurable: true, value: files });
    fireEvent.change(input);
    await tick(100);
  }
  const chips = () => [...document.querySelectorAll(".composer__file")].map((c) => c.querySelector(".composer__file-what")?.textContent);
  const draft = () => app.ui.draft(ids.pTide, NEW_CONVERSATION);
  const created = () => Object.values(app.session.commands.get()).map((c) => c.body).find((b) => b.command === "create_task");
  const type = (text: string) => fireEvent.change(screen.getAllByRole("textbox").at(-1)!, { target: { value: text } });

  it("offers only Files before a chat starts, and shows each chosen file as a chip that can be taken off", async () => {
    openAttach();
    expect(sheet().getAllByRole("button").map((b) => b.querySelector(".row__title")?.textContent).filter(Boolean)).toEqual(["Files"]);
    fireEvent.click(sheet().getByRole("button", { name: "Close" }));
    await tick(100);
    await pick(file("notes.txt", "hello"), file("data.csv", "a,b"), file("photo.jpg", "jpg", "image/jpeg"), file("log.txt"), file("build.zip", "zip", "application/zip"));
    expect(chips()).toEqual(["notes.txt5 bytes", "data.csv3 bytes", "photo.jpg3 bytes", "log.txt7 bytes", "build.zip3 bytes"]);
    expect(draft()?.files?.map((f) => f.mediaType)).toEqual(["text/plain", "text/plain", "image/jpeg", "text/plain", "application/zip"]);
    expect(stored.size).toBe(5);
    fireEvent.click(screen.getByRole("button", { name: "Remove data.csv" }));
    await tick(100);
    expect(chips()).toEqual(["notes.txt5 bytes", "photo.jpg3 bytes", "log.txt7 bytes", "build.zip3 bytes"]);
    // Its bytes no longer wait on the phone.
    expect(stored.size).toBe(4);
    // Files alone are enough to send.
    expect((screen.getByRole("button", { name: "Start conversation" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("refuses a file over the limit and says what the limit is, keeping the rest", async () => {
    await pick(sized("film.mov", MAX_FILE_BYTES + 1), file("ok.txt"));
    const notice = document.querySelector(".composer__notice")!;
    expect(notice.textContent).toContain("Not attached");
    expect(notice.textContent).toContain("“film.mov” is 100 MB. Files up to 100 MB can be attached.");
    expect(chips()).toEqual(["ok.txt6 bytes"]);
    await pick(...Array.from({ length: 10 }, (_, i) => file(`f${i}.txt`)));
    expect(chips()).toHaveLength(10);
    expect(document.querySelector(".composer__notice")!.textContent).toContain("A message can carry up to 10 files.");
  });

  it("sends the files with the first message only once they're on the computer, and shows them under it", async () => {
    await pick(file("notes.txt", "hello"), file("data.csv", "a,b"));
    type("Have a look at these");
    fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
    await tick(4000);
    const body = created();
    expect(body).toMatchObject({ conversation: true, prompt: "Have a look at these" });
    const files = (body as { bundle: { files: unknown[] } }).bundle.files;
    expect(files).toEqual([
      expect.objectContaining({ name: "notes.txt", media_type: "text/plain", size: 5 }),
      expect.objectContaining({ name: "data.csv", media_type: "text/plain", size: 3 }),
    ]);
    // Uploaded, under the computer's own ids.
    const host = window.__sim!.host;
    for (const f of files as { artifact_id: string }[]) expect(host.artifactBytes.has(f.artifact_id)).toBe(true);
    expect(vi.mocked(upload)).toHaveBeenCalledTimes(2);
    // The draft and the phone's copy are gone.
    expect(draft()?.files ?? []).toEqual([]);
    expect(stored.size).toBe(0);
    await tick(2000);
    const sent = screen.getByRole("list", { name: "2 files sent" });
    expect(within(sent).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual(["Send notes.txt to this device", "Send data.csv to this device"]);
    fireEvent.click(within(sent).getByRole("button", { name: "Send notes.txt to this device" }));
    expect(app.ui.route()).toMatchObject({ screen: "transfers" });
  });

  it("keeps the words and files when an upload fails, and trying again sends only what hadn't arrived", async () => {
    await pick(file("notes.txt", "hello"), file("data.csv", "a,b"));
    type("Have a look at these");
    const real = vi.mocked(upload).getMockImplementation()!;
    vi.mocked(upload)
      .mockImplementationOnce(real)
      .mockImplementationOnce(async () => { throw new TransferError("The computer has no room for it.", "t-2", false); });
    fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
    await tick(4000);
    expect(created()).toBeUndefined();
    expect(document.body.textContent).toContain("Couldn’t upload the files. Nothing was sent to");
    expect(document.body.textContent).toContain("The computer has no room for it.");
    // The words and both files wait; the first is already on the computer.
    expect(draft()?.text).toBe("Have a look at these");
    expect(draft()?.files?.map((f) => [f.name, !!f.artifactId])).toEqual([["notes.txt", true], ["data.csv", false]]);
    expect(chips()).toEqual(["notes.txt5 bytes", "data.csv3 bytes"]);

    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await tick(4000);
    expect(vi.mocked(upload)).toHaveBeenCalledTimes(3);
    expect(vi.mocked(upload).mock.calls.at(-1)![1]).toMatchObject({ name: "data.csv" });
    expect((created() as { bundle: { files: { name: string }[] } }).bundle.files.map((f) => f.name)).toEqual(["notes.txt", "data.csv"]);
    expect(draft()?.files ?? []).toEqual([]);
  });

  it("sends files on a later message in the same chat", async () => {
    type("Hello");
    fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
    await tick(8000);
    const chat = window.__sim!.host.tasks.find((t) => t.conversation)!;
    openAttach();
    expect(sheet().getAllByRole("button").map((b) => b.querySelector(".row__title")?.textContent).filter(Boolean)).toEqual(["Screen", "Images", "Clip", "Files", "Mark up"]);
    fireEvent.click(sheet().getByRole("button", { name: "Close" }));
    await tick(100);
    await pick(file("trace.log", "boom"));
    const sendButton = document.querySelector(".composer__send") as HTMLButtonElement;
    fireEvent.click(sendButton);
    await tick(6000);
    const later = Object.values(app.session.commands.get()).map((c) => c.body)
      .filter((b) => "task_id" in b && b.task_id === chat.task_id && (b.command === "resume_task" || b.command === "send_message"));
    expect(later.at(-1)).toMatchObject({ bundle: { files: [expect.objectContaining({ name: "trace.log", size: 4 })] } });
    expect(app.ui.draft(ids.pTide, chat.task_id)?.files ?? []).toEqual([]);
  });

  it("shows how far a slow upload has got, and the message isn't sent until it ends", async () => {
    window.__sim!.transfers.chunkDelay(3000);
    await pick(file("notes.txt", "hello"));
    fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
    await tick(200);
    expect(created()).toBeUndefined();
    expect(document.querySelector(".composer__file.is-going")).not.toBeNull();
    expect(document.body.textContent).toMatch(/Sending “notes\.txt”/);
    await tick(8000);
    expect(created()).toBeDefined();
  });
});
