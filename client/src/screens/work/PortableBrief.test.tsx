// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import { UiStore } from "../../app/ui";
import { allGrants } from "../../dev/sim/host";
import { harnesses, projects, tasks } from "../../dev/sim/scenario";
import PortableBrief from "./PortableBrief";

const fixture = vi.hoisted(() => ({
  host: null as HostState | null,
  ui: null as UiStore | null,
  save: vi.fn<(blob: Blob, name: string) => Promise<{ savedAs: string }>>(),
  command: vi.fn(),
  refresh: vi.fn<() => Promise<void>>(),
}));
vi.mock("../../app/context", async (original) => ({
  ...await original<typeof import("../../app/context")>(),
  useApp: () => ({
    ui: fixture.ui,
    profile: { profileId: "brief-test", name: "Test computer" },
    platform: { saveToPhone: fixture.save },
    session: { refreshHarnesses: fixture.refresh, command: fixture.command },
  }),
  useHost: <T,>(select: (state: HostState) => T) => {
    if (!fixture.host) throw new Error("Test host missing");
    return select(fixture.host);
  },
  useLink: () => ({ state: "online" }),
}));
vi.mock("../../design/Sheet", () => ({
  Sheet: ({ children, footer }: { children: ReactNode; footer: ReactNode }) =>
    <section>{children}{footer}</section>,
}));
const source = tasks(100_000)[1]!;
const destination = { ...projects(100_000)[1]!, active_writer: null };

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
  fixture.ui = new UiStore("brief-test");
  fixture.host = {
    ...emptyHostState, grants: allGrants, harnesses,
    projects: { [destination.project_id]: destination },
  };
  fixture.refresh.mockResolvedValue();
  fixture.save.mockResolvedValue({ savedAs: "Downloads/brief.md" });
});
afterEach(() => { fixture.ui?.persistNow(); cleanup(); });

async function chooseDestination() {
  fireEvent.click(screen.getByRole("button", { name: "Choose destination" }));
  fireEvent.change(screen.getByLabelText("Destination project"), { target: { value: destination.project_id } });
  fireEvent.change(screen.getByLabelText("Destination agent"), { target: { value: "claude_code" } });
  await waitFor(() => expect(fixture.refresh).toHaveBeenCalled());
}
describe("portable brief handoff", () => {
  it("preserves edits across remount and keeps them after a failed native save", async () => {
    const first = render(<PortableBrief task={source} onClose={() => {}} />);
    fireEvent.change(screen.getByLabelText("Brief Markdown"), { target: { value: "# Keep these edits" } });
    first.unmount();
    render(<PortableBrief task={source} onClose={() => {}} />);
    expect((screen.getByLabelText("Brief Markdown") as HTMLTextAreaElement).value).toBe("# Keep these edits");
    fixture.save.mockRejectedValueOnce(new Error("Document storage is unavailable."));
    fireEvent.click(screen.getByRole("button", { name: "Save to device" }));
    await screen.findByText("Document storage is unavailable.");
    expect(screen.queryByText("Document saved")).toBeNull();
    expect((screen.getByLabelText("Brief Markdown") as HTMLTextAreaElement).value).toBe("# Keep these edits");
    expect(fixture.command).not.toHaveBeenCalled();
  });
  it("does not overwrite another unsent new-task draft", async () => {
    fixture.ui?.saveDraft(destination.project_id, null, { text: "Existing work" });
    render(<PortableBrief task={source} onClose={() => {}} />);
    await chooseDestination();
    fireEvent.click(screen.getByRole("button", { name: "Review new task" }));
    expect(screen.getByText("A new-task draft is already waiting")).toBeTruthy();
    expect(fixture.ui?.draft(destination.project_id, null)?.text).toBe("Existing work");
    expect(fixture.command).not.toHaveBeenCalled();
  });
  it("creates only a saved new-task review draft before the explicit Start action", async () => {
    const close = vi.fn();
    render(<PortableBrief task={source} onClose={close} />);
    fireEvent.change(screen.getByLabelText("Brief Markdown"), { target: { value: "# Reviewed handoff" } });
    await chooseDestination();
    fireEvent.click(screen.getByRole("button", { name: "Review new task" }));
    expect(fixture.ui?.draft(destination.project_id, null)?.text).toBe("# Reviewed handoff");
    expect(fixture.ui?.get().stacks.talk.at(-1)).toEqual({ screen: "review", projectId: destination.project_id, taskId: null, intent: "new_task" });
    expect(close).toHaveBeenCalledOnce();
    expect(fixture.command).not.toHaveBeenCalled();
  });
});
