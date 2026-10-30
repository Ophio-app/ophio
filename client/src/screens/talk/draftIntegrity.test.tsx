// SPDX-License-Identifier: Apache-2.0
// Regression cases for draft integrity, review details and safety.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { ControlLink, LinkEvent, Platform } from "../../host/link";
import type { ClientFrame, CommandReceipt } from "../../protocol/types";
import { emptyHostState } from "../../state/projection";
import * as fixtures from "../../dev/sim/scenario";
import { Transcript } from "./Transcript";
import { send } from "./send";
import Review from "./Review";
import { RouteProvider } from "../../shell/Screen";
import SafetyStrip from "../control/SafetyStrip";

class ReviewLink implements ControlLink {
  frames: ClientFrame[] = [];
  listener: ((event: LinkEvent) => void) | undefined;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.frames.push(frame);
    return true;
  }
  listen(listener: (event: LinkEvent) => void) {
    this.listener = listener;
    return () => {
      this.listener = undefined;
    };
  }
  accept() {
    const frame = this.frames.findLast((f) => f.type === "command");
    if (!frame || frame.type !== "command")
      throw new Error("No pending command");
    const receipt: CommandReceipt = {
      command_id: frame.command_id,
      status: "accepted",
      seq: 1,
      result: null,
      error: null,
    };
    this.listener?.({ type: "frame", frame: { type: "receipt", ...receipt } });
  }
}
const sessions: HostSession[] = [];
beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
});
afterEach(() => {
  cleanup();
  for (const s of sessions.splice(0)) s.stop();
  vi.useRealTimers();
});

function setup() {
  const link = new ReviewLink(),
    session = new HostSession("draft-integrity", link);
  sessions.push(session);
  session.start();
  session.link.set({ state: "online", since: 0 });
  const app: App = {
    session,
    ui: new UiStore("draft-integrity"),
    profile: {
      profileId: "draft-integrity",
      hostId: "host",
      name: "Computer",
      deviceId: "phone",
      pairedAt: 0,
      addresses: [],
      certSha256: "",
    },
    profiles: [],
    platform: { kind: "simulated" } as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  };
  return { app, link };
}
const request = {
  projectId: "project",
  draftTaskId: "task",
  text: "First request",
  annotationId: null,
  label: "Send",
  build: () => ({
    command: "send_message" as const,
    task_id: "task",
    text: "First request",
    bundle: null,
    delivery: "steer" as const,
  }),
};

describe("draft integrity", () => {
  it("preserves text typed while the previous send awaits a receipt", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("project", "task", { text: request.text });
    const sending = send(app, request, () => {});
    app.ui.saveDraft("project", "task", { text: "A new unsent correction" });
    link.accept();
    await sending;
    expect(app.ui.draft("project", "task")?.text).toBe(
      "A new unsent correction",
    );
  });

  it("persists a command identity before a reload can lose its receipt", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("project", "task", { text: request.text });
    const sending = send(app, request, () => {});
    window.dispatchEvent(new Event("pagehide"));
    const reloaded = new UiStore("draft-integrity");
    // Finish the old promise only to clean up; the new UI was loaded before it.
    link.accept();
    await sending;
    const attempt = reloaded.draft("project", "task")?.attempt;
    expect(attempt?.commandId).toEqual(expect.any(String));
  });

  it("persists ordinary unsent text with its original project and task", () => {
    const { app } = setup();
    app.ui.saveDraft("project", "task", { text: "Keep me here" });
    app.ui.selectProject("another-project");
    window.dispatchEvent(new Event("pagehide"));
    const reloaded = new UiStore("draft-integrity");
    expect(reloaded.draft("project", "task")?.text).toBe("Keep me here");
    expect(reloaded.draft("another-project", "task")).toBeNull();
  });

  function approvalApp() {
    const { app } = setup();
    const approval = fixtures.approvals(Date.now())[0];
    if (!approval) throw new Error("No approval fixture");
    const task = fixtures
      .tasks(Date.now())
      .find((t) => t.task_id === approval.task_id);
    if (!task) throw new Error("No approval task");
    app.session.state.set({
      ...emptyHostState,
      approvals: { [approval.approval_id]: approval },
      grants: {
        view: true,
        tasks: true,
        approvals: true,
        control_input: true,
        terminal: true,
        computer_use: true,
        library_read: true,
        library_write: true,
        transfers: true,
        admin: true,
        accounts: false,
      },
      timelines: {
        [task.task_id]: {
          loaded: true,
          hasMore: false,
          items: [
            {
              seq: 1,
              at: Date.now(),
              run_id: task.current_run,
              item: { kind: "approval", approval_id: approval.approval_id },
            },
          ],
        },
      },
    });
    return { app, task };
  }

  it("keeps an inline approval reason through unmount and remount", () => {
    const { app, task } = approvalApp();
    app.session.state.set(s => ({ ...s, caughtUp: true }));
    const content = (
      <AppProvider app={app}>
        <Transcript task={task} agent="Agent" />
      </AppProvider>
    );
    const mounted = render(content);
    fireEvent.click(screen.getByRole("button", { name: "Deny…" }));
    fireEvent.change(screen.getByLabelText("Reason (optional)"), {
      target: { value: "Keep the test isolated" },
    });
    mounted.unmount();
    render(content);
    fireEvent.click(screen.getByRole("button", { name: "Deny…" }));
    expect(
      (screen.getByLabelText("Reason (optional)") as HTMLTextAreaElement).value,
    ).toBe("Keep the test isolated");
  });

  it("disables approval decisions while offline", () => {
    const { app, task } = approvalApp();
    app.session.link.set({
      state: "offline",
      since: 0,
      reason: "Network lost",
      retryAt: null,
    });
    render(
      <AppProvider app={app}>
        <Transcript task={task} agent="Agent" />
      </AppProvider>,
    );
    expect(
      (screen.getByRole("button", { name: "Allow" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });
});

describe("review details and safety", () => {
  it("keeps Stop available in the urgent slot for a coding task without a desktop lease", () => {
    const { app } = setup();
    const task = fixtures
      .tasks(Date.now())
      .find((t) => t.task_id === fixtures.ids.tFlaky);
    if (!task) throw new Error("No coding task fixture");
    app.session.state.set({
      ...emptyHostState,
      tasks: { [task.task_id]: task },
      control: {},
      grants: {
        view: true,
        tasks: true,
        approvals: true,
        control_input: true,
        terminal: true,
        computer_use: true,
        library_read: true,
        library_write: true,
        transfers: true,
        admin: true,
        accounts: false,
      },
    });
    // Shell installs this component in Sheet's UrgentSlot; the rest of the app is inert.
    render(
      <AppProvider app={app}>
        <SafetyStrip dismiss={() => {}} />
      </AppProvider>,
    );
    expect(screen.queryByRole("button", { name: /Stop/ })).not.toBeNull();
  });

  it("preserves the reviewed stopping point after leaving and reopening Review", () => {
    const { app } = setup();
    const project = fixtures
      .projects(Date.now())
      .find((p) => p.project_id === fixtures.ids.pGarden);
    if (!project) throw new Error("No project fixture");
    app.session.state.set({
      ...emptyHostState,
      projects: { [project.project_id]: project },
      harnesses: fixtures.harnesses,
    });
    app.ui.saveDraft(project.project_id, null, {
      text: "Add garden layout tests",
    });
    const route = {
      screen: "review" as const,
      projectId: project.project_id,
      taskId: null,
      intent: "new_task" as const,
    };
    const content = (
      <AppProvider app={app}>
        <RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}>
          <Review route={route} />
        </RouteProvider>
      </AppProvider>
    );
    const mounted = render(content);
    fireEvent.change(screen.getByLabelText("Stop when"), {
      target: { value: "Before changing the database" },
    });
    mounted.unmount();
    render(content);
    expect((screen.getByLabelText("Stop when") as HTMLInputElement).value).toBe(
      "Before changing the database",
    );
  });
});
