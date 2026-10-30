// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import type { Platform } from "../../host/link";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { annotationStore, type SourceDraft } from "../../state/annotations";
import { emptyHostState } from "../../state/projection";
import Annotate from "./Annotate";

vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: "blob:reviewed-image", error: null }) }));
const source: SourceDraft = { key: "S1", blobKey: "image", artifactId: "artifact", sha256: "abc", mediaType: "image/png", width: 640, height: 360,
  durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 123, derivedFrom: null, derivation: null };

beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it("counts one mark and one linked comment in the singular, and names each comment's edit button by its comment", () => {
  const profileId = "u57-counts";
  const session = new HostSession(profileId, { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} });
  session.state.set({ ...emptyHostState, readiness: { checked_at: 0, checks: [] } });
  const app: App = {
    session, ui: new UiStore(profileId), profiles: [], switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {},
    profile: { profileId, hostId: "host", deviceId: "phone", name: "Computer", addresses: [], certSha256: "", pairedAt: 0 },
    platform: { kind: "simulated", openExternal: vi.fn() } as unknown as Platform,
  };
  const store = annotationStore(profileId), d = store.create("project", null);
  store.record(d.id, v => ({ ...v, sources: [source], selectedSource: source.key }));
  const route = { screen: "annotate", annotationId: d.id } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}><Annotate route={route} /></RouteProvider></AppProvider>);

  fireEvent.click(screen.getByRole("button", { name: /^Pin$/ }));
  fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
  expect(screen.getByRole("img", { name: /^S1: image with/ }).getAttribute("aria-label"))
    .toBe("S1: image with 1 mark. Open Draft to select and describe it.");
  const field = screen.getByLabelText("What should change?");
  fireEvent.change(field, { target: { value: "Move this heading" } });
  fireEvent.click(screen.getByRole("button", { name: "Add comment" }));
  // The marks and comments are listed in the Draft sheet.
  fireEvent.click(screen.getByRole("button", { name: /^Draft/ }));
  expect(screen.getByText("1 linked comment")).toBeTruthy();

  fireEvent.change(field, { target: { value: "Make the heading the same size as the one on the settings screen" } });
  fireEvent.click(screen.getByRole("button", { name: "Add comment" }));
  expect(screen.getByText("2 linked comments")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Edit comment 1: Move this heading" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Edit comment 2: Make the heading the same size as the…" })).toBeTruthy();
});
