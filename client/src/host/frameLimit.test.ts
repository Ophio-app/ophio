// SPDX-License-Identifier: Apache-2.0
//
// The phone's native link and the companion each refuse a JSON frame over
// 1 MiB. A command that large must be refused here, saying why, instead of
// being handed to the link, which drops the connection and leaves the person
// told it may have happened.
import { expect, it } from "vitest";
import type { ClientFrame } from "../protocol/types";
import type { ControlLink, LinkEvent } from "./link";
import { CommandError, HostSession } from "./session";

class RecordingLink implements ControlLink {
  sent: ClientFrame[] = [];
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(_: (e: LinkEvent) => void) {
    return () => {};
  }
}

const save = (text: string) => ({ command: "library_save" as const, root_id: "notes", path: "log.md", text, base_revision: "rev-1", newline: "lf" as const, bom: false });

it("refuses a command too large for one frame, and sends nothing", async () => {
  const link = new RecordingLink();
  const session = new HostSession("limits", link);
  session.start();
  // Pasted text: a byte count over the limit, though fewer characters.
  const sent = session.command(save("é".repeat(600 * 1024)), { label: "Save" });
  expect(link.sent.filter((f) => f.type === "command")).toEqual([]);
  expect(session.commands.get()).toEqual({});
  const error = await sent.catch((e: unknown) => e);
  expect(error).toBeInstanceOf(CommandError);
  expect((error as CommandError).error.code).toBe("limit_exceeded");
  expect((error as CommandError).message).toBe("Nothing was sent: this is 1.2 MB and the computer accepts up to 1 MB at a time. Make it shorter.");

  // Just under the limit still goes.
  void session.command(save("x".repeat(1000 * 1024)), { label: "Save" }).catch(() => undefined);
  expect(link.sent.filter((f) => f.type === "command")).toHaveLength(1);
  session.stop();
});
