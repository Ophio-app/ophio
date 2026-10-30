// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { StageRow } from "./StageRow";

// What a phone screen reader says for the project's stage on Work.
let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
const project = () => {
  const found = window.__sim?.host.projects.find(p => p.project_id === ids.pTide);
  if (!found) throw new Error("Missing fixture");
  return found;
};
beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers();
});

it("says the project's stage, who set it and when, not only where the row goes", async () => {
  act(() => window.__sim?.host.stages.anotherDevice("review", ids.pTide));
  render(<StageRow project={project()} />, { wrapper });
  await advance(120_000);
  expect(screen.getByRole("button", { name: /^Project stage: Review\s*Another device · 2 minutes ago$/ })).toBeTruthy();
  // The actor and its separator are one stop, then the age.
  const subtitle = screen.getByText(/^Another device/).closest(".row__subtitle");
  expect([...(subtitle?.childNodes ?? [])].map(n => n.nodeName)).toEqual(["#text", "TIME"]);
});

it("says a project has no stage yet and how to set one", async () => {
  act(() => window.__sim?.host.stages.unset(ids.pTide));
  await advance();
  render(<StageRow project={project()} />, { wrapper });
  expect(screen.getByRole("button", { name: /^Stage not set\s*Choose the project’s stage$/ })).toBeTruthy();
});
