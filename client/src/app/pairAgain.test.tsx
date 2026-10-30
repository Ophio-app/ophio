// SPDX-License-Identifier: Apache-2.0
//
// Forgetting a computer, or pairing it again, makes a new pairing with a new
// profile id. The work this phone kept for that computer follows it.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { useApp } from "./context";
import { prefs } from "./prefs";
import { useStore } from "../state/store";
import { inboxStore } from "../screens/work/inbox";
import type { HostProfile } from "../host/link";
import { platform, profile } from "../platform/lifecycle.fixtures";

let fake: ReturnType<typeof platform>;
// Each test pairs anew: stores already loaded for a pairing stay in memory.
let run = 0, first: HostProfile, again: HostProfile;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/keyboard", () => ({ useKeyboardOpen: () => false, useCrampedLayout: () => false }));
vi.mock("../screens/welcome/Welcome", () => ({ Welcome: ({ onPaired }: { onPaired(p: HostProfile): void }) =>
  <button onClick={() => onPaired(again)}>Pair studio</button> }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const draft = useStore(app.ui, () => app.ui.draft("p1", null)?.text ?? "none");
  const inbox = useStore(inboxStore(app.profile.profileId), (s) => s.items.map((i) => i.text).join(", ") || "empty");
  return <div><p>Pairing {app.profile.profileId}</p><p>Draft: {draft}</p><p>Inbox: {inbox}</p>
    <button onClick={() => {
      app.ui.saveDraft("p1", null, { text: "unsent words" });
      inboxStore(app.profile.profileId).add({ kind: "text", text: "shared note", name: "Note", blobKey: null, mediaType: "text/plain", size: 11, sourceApp: null, projectId: null });
    }}>Write</button>
    <button onClick={() => void app.forget()}>Forget</button>
    <button onClick={() => app.pairAnother()}>Pair again</button>
  </div>;
} }));

beforeEach(() => {
  localStorage.clear();
  prefs.set({ ...prefs.get(), theme: "system", reduceMotion: false, lastProfileId: null });
  Element.prototype.scrollTo ??= () => {};
  run++;
  first = { ...profile, profileId: `first-${run}` };
  again = { ...profile, profileId: `again-${run}`, deviceId: "device-b", pairedAt: 2 };
  fake = platform();
  fake.value.profiles = async () => [first];
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

async function writeWork() {
  render(<App />);
  await screen.findByText(`Pairing ${first.profileId}`);
  fireEvent.click(screen.getByRole("button", { name: "Write" }));
  await screen.findByText("Draft: unsent words");
  screen.getByText("Inbox: shared note");
}

it("brings drafts and Inbox items back when a forgotten computer is paired again", async () => {
  await writeWork();
  fireEvent.click(screen.getByRole("button", { name: "Forget" }));
  fireEvent.click(await screen.findByRole("button", { name: "Pair studio" }));
  await screen.findByText(`Pairing ${again.profileId}`);
  screen.getByText("Draft: unsent words");
  screen.getByText("Inbox: shared note");
});

it("moves drafts and Inbox items to the new pairing when the computer is paired again", async () => {
  await writeWork();
  fireEvent.click(screen.getByRole("button", { name: "Pair again" }));
  fireEvent.click(await screen.findByRole("button", { name: "Pair studio" }));
  await screen.findByText(`Pairing ${again.profileId}`);
  screen.getByText("Draft: unsent words");
  screen.getByText("Inbox: shared note");
  // Work moved; the old device’s command observations stay separate.
  await act(async () => { await new Promise((r) => setTimeout(r, 400)); });
  await waitFor(() => expect(Object.keys(localStorage).filter((k) => k.includes(first.profileId) && !k.startsWith("command-history."))).toEqual([]));
});
