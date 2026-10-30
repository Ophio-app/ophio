// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { TransferRecord } from "../../protocol/types";
import { publicationPath, TransferRow } from "./TransferRow";

afterEach(cleanup);

const record: TransferRecord = {
  transfer_id: "t",
  direction: "phone_to_host",
  name: "notes.md",
  size: 5,
  sha256: "expected-sha",
  transferred: 5,
  state: { state: "reconciliation_required", reason: "Unconfirmed" },
  destination: { kind: "inbox" },
  publication: { root: "/home/sam/Inbox", path: "notes (2).md" },
  host_path: null,
  created_at: 0,
  updated_at: 1,
};

describe("unconfirmed upload publication", () => {
  it("shows where to check, offers clearing instead of resuming, and never claims success", () => {
    render(
      <TransferRow
        record={record}
        job={{
          id: "j",
          name: "notes.md",
          direction: "upload",
          size: 5,
          transferred: 5,
          phase: "paused",
          createdAt: 0,
          updatedAt: 1,
          transferId: "t",
          sha256: "expected-sha",
          destination: { kind: "inbox" },
          source: null,
          artifactId: null,
          hostPath: null,
          error: "The app closed. Continue when ready.",
          storageError: null,
          resumable: true,
        }}
        online
        onContinue={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByText(/Check on the computer/)).toBeTruthy();
    expect(screen.getByText("/home/sam/Inbox/notes (2).md")).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Clear after checking…" }),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Continue" })).toBeNull();
    expect(screen.queryByText(/Continue when ready/)).toBeNull();
    expect(screen.queryByText(/Verified/)).toBeNull();
  });

  it("says a cleared check left the computer's file alone", () => {
    render(
      <TransferRow
        record={{ ...record, state: { state: "cancelled" } }}
        online
      />,
    );
    expect(screen.getByText(/Cleared after checking/)).toBeTruthy();
    expect(
      screen.getByText(/If the file arrived, it is still there/),
    ).toBeTruthy();
  });

  it("joins the location in the computer's own path style", () => {
    expect(
      publicationPath({ root: "C:\\Users\\sam\\Inbox\\", path: "notes.md" }),
    ).toBe("C:\\Users\\sam\\Inbox\\notes.md");
    expect(publicationPath({ root: "/home/sam/", path: "docs/notes.md" })).toBe(
      "/home/sam/docs/notes.md",
    );
  });
});

it("labels internal attachment fetches without saying where the copy is", () => {
  render(<TransferRow record={{ ...record, direction: "host_to_phone", state: { state: "completed" }, shown_in_task: true }} online />);
  expect(screen.getByText(/Shown in a task/)).toBeTruthy();
  expect(screen.queryByText(/No copy is kept/)).toBeNull();
});
it("names an unnamed attachment fetch as a task attachment, never by its id", () => {
  render(<TransferRow record={{ ...record, name: "0192f7a1-0000-7000-8000-000000000301", host_path: null, direction: "host_to_phone", state: { state: "completed" }, shown_in_task: true }} online />);
  expect(screen.getByRole("heading", { name: "Task attachment" })).toBeTruthy();
});
it("says a download that isn't this phone's left no copy here", () => {
  render(<TransferRow record={{ ...record, direction: "host_to_phone", state: { state: "completed" } }} online />);
  expect(screen.getByText("No copy is kept on this device.")).toBeTruthy();
  expect(screen.queryByText(/Shown in a task/)).toBeNull();
});
it("offers to start a failed transfer over, a download only while connected", () => {
  const onRetry = vi.fn();
  const { rerender } = render(<TransferRow record={{ ...record, direction: "host_to_phone", state: { state: "failed", reason: "Gone" } }} online onRetry={onRetry} />);
  fireEvent.click(screen.getByRole("button", { name: "Download again" }));
  expect(onRetry).toHaveBeenCalledOnce();
  rerender(<TransferRow record={{ ...record, direction: "host_to_phone", state: { state: "failed", reason: "Gone" } }} online={false} onRetry={onRetry} />);
  expect((screen.getByRole("button", { name: "Download again" }) as HTMLButtonElement).disabled).toBe(true);
  rerender(<TransferRow record={{ ...record, state: { state: "failed", reason: "Gone" } }} online={false} onRetry={onRetry} />);
  expect((screen.getByRole("button", { name: "Send again…" }) as HTMLButtonElement).disabled).toBe(false);
});
it("doesn't offer to attach an upload made for a message, and says where to send it instead", () => {
  render(
    <TransferRow
      record={{ ...record, state: { state: "completed" }, destination: { kind: "artifact" }, publication: undefined }}
      job={{
        id: "j",
        name: "notes.md",
        direction: "upload",
        size: 5,
        transferred: 5,
        phase: "completed",
        createdAt: 0,
        updatedAt: 1,
        transferId: "t",
        sha256: "expected-sha",
        destination: { kind: "artifact" },
        source: null,
        artifactId: "0192f7a1-0000-7000-8000-000000000301",
        hostPath: null,
        error: null,
        storageError: null,
        resumable: false,
      }}
      online
    />,
  );
  expect(screen.getByText(/Messages can’t attach files sent from here/)).toBeTruthy();
  expect(screen.getByText(/send it again to the Inbox or a project folder/)).toBeTruthy();
  expect(screen.queryByText(/Ready to attach/)).toBeNull();
  expect(screen.queryByText(/0192f7a1-0000-7000-8000-000000000301/)).toBeNull();
});
