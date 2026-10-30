// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { approvalCases as cases, ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { Approval } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { AttentionList } from "./shared";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
const codeIn = (root: Element) => [...root.querySelectorAll("code")].map(code => code.textContent);

it("sets the command, pattern and path of a request as code in the Needs you row, a grouped row and the sheet", async () => {
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><AttentionList projectId={ids.pTide} /></RouteProvider></AppProvider>);
  const quick = document.querySelector(".talk-quick-approval__detail")!;
  expect(codeIn(quick)).toEqual(["printf fixture"]);
  expect(quick.textContent).toContain("Print fixture text");

  const host = window.__sim!.host;
  const first = host.approvals.find(a => a.approval_id === ids.aTest)!;
  const item = host.attention.find(a => a.attention_id === ids.atApproval)!;
  const grep = cases.find(c => c.tool === "Grep")!.approval;
  const second: Approval = { ...first, ...grep, kind: "tool", approval_id: crypto.randomUUID(), native_request_id: "toolu_grep", requested_at: Date.now() };
  act(() => {
    host.emit({ type: "approval_upserted", approval: second });
    host.emit({ type: "attention_upserted", item: { ...item, attention_id: crypto.randomUUID(), approval_id: second.approval_id, created_at: Date.now() } });
  });
  await advance();
  fireEvent.click(screen.getByText(/^2 approvals/));
  const row = screen.getByText("TODO").closest("button")!;
  expect(codeIn(row)).toEqual(["TODO", "src"]);
  expect(row.textContent).toContain("Pattern: TODO");

  fireEvent.click(row);
  const sheet = within(screen.getByRole("dialog"));
  expect(sheet.getByRole("heading", { name: "Use Grep" })).toBeTruthy();
  expect(codeIn(screen.getByRole("dialog").querySelector(".approval__detail")!)).toEqual(["TODO", "src"]);
});

it("keeps a proposed change out of a grouped row and shows it in the sheet", async () => {
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><AttentionList projectId={ids.pTide} /></RouteProvider></AppProvider>);
  const host = window.__sim!.host;
  const first = host.approvals.find(a => a.approval_id === ids.aTest)!;
  const item = host.attention.find(a => a.attention_id === ids.atApproval)!;
  const edit = cases.find(c => (c.input as { file_path?: string }).file_path === "client/src/screens/Home.tsx")!.approval;
  const second: Approval = { ...first, ...edit, kind: "file_edit", approval_id: crypto.randomUUID(), native_request_id: "toolu_edit", requested_at: Date.now() };
  act(() => {
    host.emit({ type: "approval_upserted", approval: second });
    host.emit({ type: "attention_upserted", item: { ...item, attention_id: crypto.randomUUID(), approval_id: second.approval_id, created_at: Date.now() } });
  });
  await advance();
  fireEvent.click(screen.getByText(/^2 approvals/));
  const row = screen.getByText("client/src/screens/Home.tsx").closest("button")!;
  expect(row.textContent).toContain("Replace one match: 126 → 157 characters");
  expect(row.querySelector(".approval__change")).toBeNull();

  fireEvent.click(row);
  const sheet = screen.getByRole("dialog");
  expect(within(sheet).getByRole("group", { name: "Proposed change" }).textContent).toContain('Added:   if (!tasks.length) return <Empty title="Nothing waiting" />;');
});
