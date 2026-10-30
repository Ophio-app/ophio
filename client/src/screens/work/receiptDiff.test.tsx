// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Receipt from "./Receipt";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("shows a reported diff as wrapped, signed and coloured lines with changed words marked", async () => {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.task_id === workIds.receipt && r.outcome)!;
  const long = "A long paragraph that a phone must wrap rather than scroll sideways. ".repeat(4).trim();
  const text = ["--- a/notes.md", "+++ b/notes.md", "@@ -1,3 +1,3 @@", " # Notes", "---- an old rule",
    `-${long} Keep the line endings.`, `+${long} Keep the CRLF line endings.`, "\\ No newline at end of file"].join("\n");
  const outcome = { ...run.outcome!, changed_files: [{ path: "notes.md", change: "modified" as const, added: 1, removed: 2,
    provenance: "native" as const, diff: { text, truncated: true } }] };
  host.runs = host.runs.map(r => r === run ? { ...run, outcome } : r);
  host.emit({ type: "run_upserted", run: { ...run, outcome } });
  const route = { screen: "receipt", taskId: workIds.receipt } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Receipt route={route} /></RouteProvider></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(250); });
  fireEvent.click(screen.getByRole("button", { name: /notes\.md.*Open the recorded diff/ }));
  const sheet = within(screen.getByRole("dialog", { name: /Agent-reported diff/ }));
  expect(sheet.getByText("Only the beginning of this diff was retained.")).toBeTruthy();
  const region = sheet.getByRole("region", { name: "Recorded file changes" });
  const lines = [...region.querySelectorAll(".diff__line")].map(l => [l.className.replace("diff__line diff__line--", ""),
    l.querySelector(".diff__sign")!.textContent, l.querySelector(".diff__text")!.textContent]);
  expect(lines).toEqual([
    ["file", "", "--- a/notes.md"],
    ["file", "", "+++ b/notes.md"],
    ["hunk", "", "@@ -1,3 +1,3 @@"],
    ["same", "", "# Notes"],
    ["remove", "−", "Removed: --- an old rule"],
    ["remove", "−", `Removed: ${long} Keep the line endings.`],
    ["add", "+", `Added: ${long} Keep the CRLF line endings.`],
    ["note", "", "\\ No newline at end of file"],
  ]);
  expect([...region.querySelectorAll(".diff__word")].map(w => w.textContent)).toEqual(["CRLF "]);
});
