// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it, vi } from "vitest";
import { SimHost } from "../../dev/sim/host";
import { ids } from "../../dev/sim/scenario";
import type { Command, InstructionSource } from "../../protocol/types";
import { instructionStatus } from "./Instructions";
const hosts: SimHost[] = [];
afterEach(() => {
  for (const host of hosts.splice(0)) host.dispose();
  vi.useRealTimers();
});
const host = () => {
  const h = new SimHost();
  hosts.push(h);
  return h;
};
const command = (h: SimHost, body: Command) =>
  h.command({
    body,
    command_id: crypto.randomUUID(),
    expected_revision: null,
    expires_at: null,
    issued_at: Date.now(),
  });
describe("Library review flows", () => {
  it("retains deletion metadata and refuses restoration over an occupant", async () => {
    const h = host();
    const deleted = await h.libraryWorkflows.deleted(
      ids.rootInstr,
      "README.md",
      "old content",
    );
    if (deleted.kind !== "library_deleted")
      throw new Error("missing deletion identity");
    const q = await h.query({ kind: "library_trash" });
    expect(q).toMatchObject({
      items: [{ trash_id: deleted.item.trash_id, path: "README.md", size: 11 }],
    });
    const refused = await command(h, {
      command: "library_restore",
      trash_id: deleted.item.trash_id,
      base_revision: deleted.item.revision,
      copy_path: null,
    });
    expect(refused.status).toBe("rejected");
    const restored = await command(h, {
      command: "library_restore",
      trash_id: deleted.item.trash_id,
      base_revision: deleted.item.revision,
      copy_path: "README-copy.md",
    });
    expect(restored.result).toMatchObject({
      kind: "library_restored",
      path: "README-copy.md",
    });
    expect(h.files[ids.rootInstr]?.["README-copy.md"]).toBe("old content");
  });
  it("moves a reviewed file into another folder and keeps other files intact", async () => {
    const h = host();
    const q = await h.query({
      kind: "library_read",
      root_id: ids.rootInstr,
      path: "README.md",
    });
    if (q.kind !== "library_read") throw new Error("file missing");
    await command(h, {
      command: "library_rename",
      root_id: ids.rootInstr,
      from: "README.md",
      to: "docs/README.md",
      base_revision: q.file.revision,
    });
    await vi.waitFor(() =>
      expect(h.files[ids.rootInstr]?.["README.md"]).toBeUndefined(),
    );
    expect(h.files[ids.rootInstr]?.["docs/README.md"]).toBe(q.file.text);
    expect(h.files[ids.rootInstr]?.["AGENTS.md"]).toBeTruthy();
  });
  it("separates discovery, loaded evidence, and changed revisions", () => {
    const source: InstructionSource = {
      label: "Instructions",
      root_id: null,
      path: null,
      host_path: "/project/AGENTS.md",
      harness: "codex",
      scope: "project",
      state: "changed",
      loaded_reported: false,
      revision_at_start: "old",
      current_revision: "new",
    };
    expect(instructionStatus(source)).toBe("Discovered, not confirmed loaded");
    expect(instructionStatus({ ...source, loaded_reported: true })).toBe(
      "Agent reported loading this source",
    );
    expect(instructionStatus({ ...source, state: "unknown" })).toContain(
      "couldn’t inspect",
    );
    expect(instructionStatus({ ...source, state: "not_applied" })).toBe(
      "Runs of this project don’t read this source",
    );
    const settings: InstructionSource = {
      ...source,
      label: "Project settings",
      scope: "settings",
      state: "not_applied",
      revision_at_start: null,
      current_revision: null,
    };
    expect(instructionStatus(settings)).toBe("Work runs ignore these settings");
    expect(instructionStatus({ ...settings, state: "discovered" })).toBe(
      "Work runs use these settings",
    );
    expect(instructionStatus({ ...settings, state: "missing" })).toBe(
      "Not found",
    );
  });
});
