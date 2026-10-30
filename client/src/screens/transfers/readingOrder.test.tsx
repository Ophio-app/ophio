// SPDX-License-Identifier: Apache-2.0
//
// Transfers reads in the order it shows: the form to send a file, then what
// is moving or waiting on the person, then the history, folded. Nothing is
// inserted above the form, so a field being typed in keeps its focus as
// transfers arrive, and a transfer being watched stays where it is as it
// finishes.
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import type { TransferRecord } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Transfers from "./Transfers";
import { transferManager } from "./manager";

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
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "library", route: { screen: "transfers" }, depth: 1, active: true }}>{children}</RouteProvider>
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

const received = (id: string): TransferRecord => ({
  transfer_id: id, direction: "phone_to_host", name: `${id}.txt`, size: 1, transferred: 1, sha256: null,
  state: { state: "completed" }, destination: { kind: "inbox" }, destination_folder: "Inbox", host_path: `Inbox/${id}.txt`,
  created_at: 0, updated_at: 0,
});
const headings = () => screen.getAllByRole("heading").map((h) => h.textContent);
const before = (a: string, b: string) => {
  const all = headings();
  return all.indexOf(a) >= 0 && all.indexOf(a) < all.indexOf(b);
};

const showHistory = () => fireEvent.click(screen.getByRole("button", { name: /^Show \d+$/ }));

it("reads the form first, and folds the history under it", async () => {
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
  await advance();
  expect(before("Send a file", "History")).toBe(true);
  expect(screen.getByText("No transfers yet")).toBeTruthy();

  await act(async () => { window.__sim!.host.emit({ type: "transfer_upserted", transfer: received("notes") }); });
  await advance();
  expect(before("Send a file", "History")).toBe(true);
  expect(screen.queryByText("notes.txt")).toBeNull();
  const toggle = screen.getByRole("button", { name: "Show 1" });
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  showHistory();
  expect(screen.getByText("notes.txt")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Hide" }).getAttribute("aria-expanded")).toBe("true");
});

it("keeps the field being typed in focused as transfers arrive", async () => {
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
  await advance();
  fireEvent.click(screen.getByRole("radio", { name: "Text" }));
  await advance();
  const field = screen.getByRole("textbox", { name: /name/i });
  field.focus();
  expect(document.activeElement).toBe(field);

  await act(async () => {
    window.__sim!.host.emit({ type: "transfer_upserted", transfer: { ...received("moving"), transferred: 0, size: 10, state: { state: "in_progress" } } });
  });
  await advance();
  expect(before("Send a file", "Current transfers")).toBe(true);
  expect(document.activeElement).toBe(field);
});

it("shows what was just sent under the form, and keeps it there as it finishes", async () => {
  const shown: Element[] = [];
  Element.prototype.scrollIntoView = function (this: Element) { shown.push(this); };
  try {
    render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
    await advance();
    fireEvent.click(screen.getByRole("radio", { name: "Text" }));
    await advance();
    fireEvent.change(screen.getByRole("textbox", { name: /File name/ }), { target: { value: "plan.md" } });
    fireEvent.change(screen.getByRole("textbox", { name: /File text/ }), { target: { value: "Ship it" } });
    await advance();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    for (let i = 0; i < 30 && shown.length === 0; i++) await advance(50);

    expect(shown).toHaveLength(1);
    const current = shown[0]!;
    expect(current.isConnected).toBe(true);
    expect(current.querySelector("h2")?.textContent).toBe("Current transfers");
    const form = screen.getByRole("heading", { name: "Send a file" });
    expect(current.compareDocumentPosition(form) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();

    for (let i = 0; i < 40 && !screen.queryByText(/Completed/); i++) await advance(100);
    expect(screen.getByText(/Completed/)).toBeTruthy();
    // Finished while watched: still under Current transfers, not folded away.
    expect(current.textContent).toContain("plan.md");
    expect(shown).toHaveLength(1);
  } finally {
    delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  }
});

it("counts attachments fetched to show in tasks rather than listing them", async () => {
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
  await advance();
  await act(async () => {
    window.__sim!.host.emit({ type: "transfer_upserted", transfer: { ...received("pic"), direction: "host_to_phone", shown_in_task: true } });
    window.__sim!.host.emit({ type: "transfer_upserted", transfer: received("notes") });
  });
  await advance();
  showHistory();
  expect(screen.getByText("notes.txt")).toBeTruthy();
  expect(screen.queryByText("pic.txt")).toBeNull();
  expect(screen.getByText("1 attachment loaded to show in a task isn’t listed.")).toBeTruthy();
});

it("searches a long history by name or folder", async () => {
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
  await advance();
  await act(async () => {
    for (const id of ["a", "b", "c", "d", "e", "budget"])
      window.__sim!.host.emit({ type: "transfer_upserted", transfer: received(id) });
  });
  await advance();
  expect(screen.queryByRole("searchbox", { name: "Search transfers" })).toBeNull();
  showHistory();
  fireEvent.change(screen.getByRole("searchbox", { name: "Search transfers" }), { target: { value: "budg" } });
  expect(screen.getByText("budget.txt")).toBeTruthy();
  expect(screen.queryByText("a.txt")).toBeNull();
  fireEvent.change(screen.getByRole("searchbox", { name: "Search transfers" }), { target: { value: "nothing" } });
  expect(screen.getByText("No matching transfers")).toBeTruthy();
});

it("starts a failed download over, and the failure moves to the history", async () => {
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
  await advance();
  const manager = transferManager(app);
  const source = { kind: "artifact" as const, artifact_id: "0192f7a1-0000-7000-8000-00000000dead" };
  let first = "";
  await act(async () => { first = manager.startDownload(source, "receipt.png"); });
  for (let i = 0; i < 20 && manager.get().jobs[first]?.phase !== "failed"; i++) await advance(100);
  expect(manager.get().jobs[first]?.phase).toBe("failed");
  const start = vi.spyOn(manager, "startDownload");
  fireEvent.click(screen.getByRole("button", { name: "Download again" }));
  expect(start).toHaveBeenCalledWith(source, "receipt.png");
  expect(manager.get().jobs[first]?.retried).toBe(true);
  await advance();
  // The new attempt is the one under Current transfers; the first is history.
  expect(screen.getAllByRole("button", { name: "Download again" })).toHaveLength(1);
  expect(screen.getByRole("button", { name: /^Show \d+$/ })).toBeTruthy();
});

it("keeps Home out of the projects a file can be sent to", async () => {
  render(<Transfers route={{ screen: "transfers" }} />, { wrapper });
  await advance();
  fireEvent.click(screen.getByRole("radio", { name: "A project folder" }));
  await advance();
  fireEvent.click(screen.getByRole("button", { name: /^Project: .*Choose another$/ }));
  await advance();
  const dialog = screen.getByRole("dialog", { name: "Send to a project" });
  const names = [...dialog.querySelectorAll(".row__title")].map((t) => t.textContent);
  expect(names.length).toBeGreaterThan(0);
  expect(names).not.toContain("Home");
  fireEvent.change(screen.getByRole("searchbox", { name: "Search projects" }), { target: { value: "zzz" } });
  expect(screen.getByText("No matching projects")).toBeTruthy();
});
