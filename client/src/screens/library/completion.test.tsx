// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { LibraryStore, libraryStore } from "./drafts";
import { stageRecovery } from "./recovery";
import { saveDraft } from "./save";
import { FileProvenance, readerContext } from "./Provenance";
import Reusable from "./Reusable";
import Recovery from "./Recovery";

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
async function snapshot() {
  const recoveryId = host().libraryCompletion.seed();
  const r = await host().query({
    kind: "library_recovery_read",
    root_id: ids.rootNotes,
    recovery_id: recoveryId,
  });
  if (r.kind !== "library_recovery_read") throw new Error("Missing recovery");
  return r.recovery;
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

it("stages a retained choice and saves only through the reviewed revision path", async () => {
  const recovery = await snapshot(),
    store = libraryStore(app.profile.profileId);
  await run(() => stageRecovery(app.session, store, recovery, "proposed"));
  expect(Object.values(app.session.commands.get())).toHaveLength(0);
  const draft = store.draft(ids.rootNotes, recovery.item.path);
  if (!draft || recovery.current.state !== "text")
    throw new Error("No staged draft");
  expect(draft.base.revision).toBe(recovery.current.file.revision);
  expect((await run(() => saveDraft(app.session, store, draft))).kind).toBe(
    "saved",
  );
  expect(
    host().libraryCompletion.recoveries.has(recovery.item.recovery_id),
  ).toBe(true);
});
it("refuses a changed destination and preserves the choice without writing", async () => {
  const recovery = await snapshot(),
    store = libraryStore(app.profile.profileId);
  store.form(`recovery-choice:${recovery.item.recovery_id}`, "proposed");
  await host().library.editOnComputer(
    ids.rootNotes,
    recovery.item.path,
    "someone else wrote this",
  );
  await run(async () => {
    await expect(
      stageRecovery(app.session, store, recovery, "proposed"),
    ).rejects.toThrow("destination changed");
  });
  expect(Object.values(app.session.commands.get())).toHaveLength(0);
  expect(
    store.get().forms[`recovery-choice:${recovery.item.recovery_id}`],
  ).toBe("proposed");
  expect(host().files[ids.rootNotes]?.[recovery.item.path]).toBe(
    "someone else wrote this",
  );
});
it("recovery staging never replaces a different unsent draft", async () => {
  const recovery = await snapshot(),
    store = libraryStore(app.profile.profileId);
  if (recovery.current.state !== "text") throw new Error("No destination");
  store.saveDraft({
    base: recovery.current.file,
    text: "other unsent work",
    updatedAt: 1,
    phase: "draft",
  });
  await expect(
    stageRecovery(app.session, store, recovery, "proposed"),
  ).rejects.toThrow("already has a draft");
  expect(
    new LibraryStore(app.profile.profileId).draft(
      ids.rootNotes,
      recovery.item.path,
    )?.text,
  ).toBe("other unsent work");
});
it("discard review names the file and only explicit confirmation removes the copy", async () => {
  const recovery = await snapshot(),
    before = host().files[ids.rootNotes]?.[recovery.item.path];
  render(
    <Recovery
      route={{
        screen: "library_recovery",
        rootId: ids.rootNotes,
        recoveryId: recovery.item.recovery_id,
      }}
    />,
    { wrapper },
  );
  await advance();
  fireEvent.click(
    screen.getByRole("button", { name: "Discard this recovery copy…" }),
  );
  expect(screen.getByRole("dialog").textContent).toContain("notes/plan.md");
  expect(host().libraryCompletion.recoveries.size).toBe(1);
  fireEvent.click(
    screen.getByRole("button", {
      name: "Discard the recovery copy of notes/plan.md",
    }),
  );
  await advance();
  expect(host().libraryCompletion.recoveries.size).toBe(0);
  expect(host().files[ids.rootNotes]?.[recovery.item.path]).toBe(before);
});
it("reader preserves task discovery and changed evidence with age, but Library has no task claim", async () => {
  const route = {
    screen: "file" as const,
    rootId: ids.rootInstr,
    path: "AGENTS.md",
    taskId: ids.tHeaders,
    source: "instructions" as const,
  };
  const view = render(<FileProvenance route={route} />, { wrapper });
  await advance();
  expect(screen.getByText("Instruction file discovered")).toBeTruthy();
  expect(screen.getByText(/Not confirmed by the agent/)).toBeTruthy();
  expect(
    screen.getByText(/Starting revision/).querySelector("time"),
  ).toBeTruthy();
  await act(async () => {
    await host().library.editOnComputer(
      ids.rootInstr,
      "AGENTS.md",
      "new guidance",
    );
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh instruction status" }),
  );
  await advance();
  expect(
    screen.getByText("Changed since this task’s run started"),
  ).toBeTruthy();
  view.unmount();
  render(
    <FileProvenance
      route={{ screen: "file", rootId: ids.rootInstr, path: "AGENTS.md" }}
    />,
    { wrapper },
  );
  expect(
    screen.queryByText(/Instruction file discovered|Changed since/),
  ).toBeNull();
});
it("native previews preserve independent edits, refuse collisions and save only to Drafts", async () => {
  const view = render(
    <Reusable route={{ screen: "reusable", taskId: ids.tFlaky }} />,
    { wrapper },
  );
  await advance();
  fireEvent.change(screen.getByLabelText("Reusable kind"), {
    target: { value: "skill" },
  });
  expect(
    (screen.getByLabelText("Editable preview") as HTMLTextAreaElement).value,
  ).toContain("disable-model-invocation: true");
  fireEvent.change(screen.getByLabelText("Editable preview"), {
    target: { value: "---\nname: reviewed\n---\nMy edited skill" },
  });
  fireEvent.change(screen.getByLabelText("Draft destination"), {
    target: { value: "sim-drafts" },
  });
  fireEvent.change(screen.getByLabelText("Draft file name"), {
    target: { value: "chosen.md" },
  });
  const files = host().files["sim-drafts"];
  if (!files) throw new Error("No Drafts root");
  files["chosen.md"] = "occupied";
  fireEvent.click(screen.getByRole("button", { name: "Save reviewed draft" }));
  await advance();
  expect(screen.getByText(/Choose a different draft file name/)).toBeTruthy();
  expect(files["chosen.md"]).toBe("occupied");
  view.unmount();
  render(<Reusable route={{ screen: "reusable", taskId: ids.tFlaky }} />, {
    wrapper,
  });
  await advance();
  expect(
    (screen.getByLabelText("Editable preview") as HTMLTextAreaElement).value,
  ).toContain("My edited skill");
  fireEvent.change(screen.getByLabelText("Draft file name"), {
    target: { value: "reviewed.md" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Save reviewed draft" }));
  await advance();
  expect(files["reviewed.md"]).toContain("My edited skill");
  fireEvent.change(screen.getByLabelText("Agent"), {
    target: { value: "codex" },
  });
  await advance();
  expect(screen.getByText(/No verified template/)).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Reusable kind"), {
    target: { value: "subagent" },
  });
  expect(
    (screen.getByLabelText("Editable preview") as HTMLTextAreaElement).value,
  ).toContain("developer_instructions =");
});
it("a receipt file inherits only its immediately preceding task context", () => {
  const file = {
    screen: "file" as const,
    rootId: ids.rootInstr,
    path: "AGENTS.md",
  };
  expect(
    readerContext(file, { screen: "receipt", taskId: ids.tHeaders }),
  ).toEqual({ ...file, taskId: ids.tHeaders, source: "receipt" });
  expect(readerContext(file, { screen: "library" })).toBe(file);
  expect(readerContext(file, { screen: "talk" })).toBe(file);
  const explicit = {
    ...file,
    taskId: ids.tFlaky,
    source: "instructions" as const,
  };
  expect(
    readerContext(explicit, { screen: "receipt", taskId: ids.tHeaders }),
  ).toBe(explicit);
});

it("an edited native preview and its name remain usable after reopening offline", async () => {
  const view = render(
    <Reusable route={{ screen: "reusable", taskId: ids.tHeaders }} />,
    { wrapper },
  );
  await advance();
  fireEvent.change(screen.getByLabelText("Reusable kind"), {
    target: { value: "subagent" },
  });
  fireEvent.change(screen.getByLabelText("Editable preview"), {
    target: { value: 'name = "kept_task"\n' },
  });
  view.unmount();
  window.__sim?.offline();
  await advance();
  render(<Reusable route={{ screen: "reusable", taskId: ids.tHeaders }} />, {
    wrapper,
  });
  expect(
    (screen.getByLabelText("Editable preview") as HTMLTextAreaElement).value,
  ).toBe('name = "kept_task"\n');
  expect(
    (screen.getByLabelText("Draft file name") as HTMLInputElement).value,
  ).toBe("agent-draft.toml.md");
  fireEvent.change(screen.getByLabelText("Editable preview"), {
    target: { value: "More local edits" },
  });
  expect(
    (
      screen.getByRole("button", {
        name: "Save reviewed draft",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(
    Object.values(libraryStore(app.profile.profileId).get().forms),
  ).toContain("More local edits");
});

it("keeps on-disk text with one explicit discard and never stages an editor draft", async () => {
  const recovery = await snapshot();
  const before = host().files[ids.rootNotes]?.[recovery.item.path];
  render(<Recovery route={{ screen: "library_recovery", rootId: ids.rootNotes, recoveryId: recovery.item.recovery_id }} />, { wrapper });
  await advance();
  fireEvent.click(screen.getAllByRole("radio", { name: "On disk now" })[0]!);
  expect(host().libraryCompletion.recoveries.size).toBe(1);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByRole("button", { name: "Discard this recovery copy…" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Keep what's on disk · discard the recovery copy" }));
  await advance();
  expect(host().libraryCompletion.recoveries.size).toBe(0);
  expect(host().files[ids.rootNotes]?.[recovery.item.path]).toBe(before);
  expect(libraryStore(app.profile.profileId).draft(ids.rootNotes, recovery.item.path)).toBeNull();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByText("Recovery copy discarded")).toBeTruthy();
});
