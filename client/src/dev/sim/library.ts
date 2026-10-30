// SPDX-License-Identifier: Apache-2.0
import type { Event, LibraryRoot } from "../../protocol/types";
import { ids } from "./scenario";
interface LibraryHost {
  roots: LibraryRoot[];
  files: Record<string, Record<string, string>>;
  emit(event: Event): number;
}
export class SimLibrary {
  readonly modified = new Map<string, number>();
  private uncertain = false;
  private errors = new Map<string, string>();
  constructor(private host: LibraryHost) {
    host.roots.push(
      {
        root_id: "sim-reference",
        label: "Reference",
        host_path: "/home/sam/Shared/reference",
        kind: "notes",
        writable: false,
        project_id: null,
        harness: null,
      },
      {
        root_id: "sim-empty",
        label: "New notebook",
        host_path: "/home/sam/Notes/new",
        kind: "notes",
        writable: true,
        project_id: null,
        harness: null,
      },
    );
    host.files["sim-reference"] = {
      "handbook.md":
        "# Team handbook\n\nKeep changes small and name the behavior they improve.\n\n## Review\n\nRead the diff, run the checks, and record what is still uncertain.\n",
    };
    host.files["sim-empty"] = {};
    Object.assign(host.files[ids.rootInstr] ?? {}, {
      "docs/guides/release.md":
        "# Release checklist\n\nKeep the change small.\n\n## Before you ship\n\n- Read the changes.\n- Run the checks.\n- Write clear release notes.\n\nA release is ready after the computer confirms each check.\n",
      ".hidden.md":
        "# Local notes\n\nHidden files stay in their original folder.\n",
      "docs/archive.md":
        "# Archive\n\n" +
        "A historical release entry with details and links.\n".repeat(14000),
      "docs/metrics.csv": "route,latency_ms\n/orders,24\n/payments,38\n",
      "docs/windows.md": "# Windows notes\r\n\r\nKeep the line endings.\r\n",
      "docs/mixed.md": "# Mixed endings\r\n\nEdit on the computer.\r\n",
    });
  }
  assertRead(rootId: string) {
    if (!this.host.roots.some((r) => r.root_id === rootId))
      throw { code: "not_found", message: "This root is no longer shared." };
    const message = this.errors.get(rootId);
    if (message) throw { code: "permission_denied", message };
  }
  assertWrite(rootId: string) {
    this.assertRead(rootId);
    if (!this.host.roots.find((r) => r.root_id === rootId)?.writable)
      throw { code: "permission_denied", message: "This folder is read only." };
  }
  async editOnComputer(
    rootId: string,
    path: string,
    text: string,
  ): Promise<void> {
    const files = this.host.files[rootId];
    if (!files) throw new Error("No such simulated root");
    files[path] = text;
    this.modified.set(`${rootId}/${path}`, Date.now());
    const bytes = new TextEncoder().encode(text),
      hash = await crypto.subtle.digest("SHA-256", bytes);
    const revision = [...new Uint8Array(hash)]
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
    this.host.emit({
      type: "library_file_changed",
      root_id: rootId,
      path,
      revision,
    });
  }
  uncertainNextSave(): void {
    this.uncertain = true;
  }
  consumeUncertain(): boolean {
    const next = this.uncertain;
    this.uncertain = false;
    return next;
  }
  deny(
    rootId: string,
    message = "The computer no longer allows reading this folder.",
  ) {
    this.errors.set(rootId, message);
  }
  allow(rootId: string) {
    this.errors.delete(rootId);
  }
}
