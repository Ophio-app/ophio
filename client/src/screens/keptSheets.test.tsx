// SPDX-License-Identifier: Apache-2.0
//
// Sheets holding something typed and not yet sent outlast another screen
// coming in front, and are back as they were left on Back.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { SurfaceActive } from "../design/surface";
import { HostSession } from "../host/session";
import { RenameScreen } from "./control/RenameScreen";
import { AddService } from "./settings/AddService";
import { BranchSheet } from "./talk/BranchPicker";

let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

/** Renders `node`, then hides its screen and shows it again, as a tapped notification and Back do. */
function coveredAndBack(node: ReactNode) {
  const tree = (active: boolean) => <AppProvider app={app}><SurfaceActive.Provider value={active}>{node}</SurfaceActive.Provider></AppProvider>;
  const view = render(tree(true));
  return () => {
    view.rerender(tree(false));
    expect(document.querySelector("dialog[open]")).toBeNull();
    view.rerender(tree(true));
  };
}

it("keeps a screen's typed name", () => {
  const target = app.session.state.get().targets.find((t) => t.target_id === ids.tgHdmi)!;
  const close = vi.fn();
  const back = coveredAndBack(<RenameScreen target={target} onClose={close} />);
  fireEvent.change(screen.getByLabelText("Screen name"), { target: { value: "Desk monitor" } });
  back();
  expect(screen.getByLabelText<HTMLInputElement>("Screen name").value).toBe("Desk monitor");
  expect(close).not.toHaveBeenCalled();
});

it("keeps a service being described, which holds no key", () => {
  const close = vi.fn();
  const back = coveredAndBack(<AddService open online onClose={close} onAdd={async () => true} />);
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Groq" } });
  fireEvent.change(screen.getByLabelText("Environment variable"), { target: { value: "GROQ_API_KEY" } });
  back();
  expect(screen.getByLabelText<HTMLInputElement>("Name").value).toBe("Groq");
  expect(screen.getByLabelText<HTMLInputElement>("Environment variable").value).toBe("GROQ_API_KEY");
  expect(close).not.toHaveBeenCalled();
});

it("keeps a new branch's name being typed", () => {
  const branches = { listing: { current: "main", branches: ["main"], dirty_files: 0, truncated: false }, read: true, loading: false, failed: false, refresh() {} };
  const pick = vi.fn();
  function Picker() {
    const [open, setOpen] = useState(true);
    return <BranchSheet open={open} onClose={() => setOpen(false)} branches={branches} chosen={null} onPick={pick} computer="studio" />;
  }
  const back = coveredAndBack(<Picker />);
  fireEvent.click(screen.getByText("New branch"));
  fireEvent.change(screen.getByLabelText("Branch name"), { target: { value: "fix/retry-cap" } });
  back();
  expect(screen.getByLabelText<HTMLInputElement>("Branch name").value).toBe("fix/retry-cap");
  fireEvent.click(screen.getByRole("button", { name: "Use new branch" }));
  expect(pick).toHaveBeenCalledWith({ name: "fix/retry-cap", new: true });
});
