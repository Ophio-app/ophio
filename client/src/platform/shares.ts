// SPDX-License-Identifier: Apache-2.0
import type { SharedItem } from "../host/link";
import { announce } from "../design/Announcer";
import { InboxError, InboxStore, inboxStore } from "../screens/work/inbox";
import { reportPlatformError } from "./errors";

export const MAX_SHARE_BYTES = 50 * 1024 * 1024;
export const MAX_SHARE_TEXT_BYTES = 256 * 1024;
export const PHONE_INBOX = "phone-holding";

/** Serialized intake prevents two deliveries of the same id racing blob writes.
 * The holding inbox also retains the durable intake ledger after items move. */
export class ShareIntake {
  #tail: Promise<void> = Promise.resolve();
  constructor(
    private readonly store: (profileId: string) => InboxStore = inboxStore,
  ) {}

  receive(items: SharedItem[], profileId: string | null): Promise<void> {
    return this.#queue(async () => {
      const holding = this.store(PHONE_INBOX);
      let added = false;
      let storageFailure: unknown;
      for (const item of items) {
        try {
          if (item.id && holding.hasIntake(item.id)) {
            if (holding.get().error) holding.retrySave();
            continue;
          }
          if (item.error) throw new Error(item.error);
          const base = {
            name: item.name || "Shared item",
            sourceApp: null,
            projectId: null,
            intakeId: item.id,
            destinationProfileId: profileId,
          };
          if (item.bytes) {
            if (item.bytes.byteLength > MAX_SHARE_BYTES)
              throw new Error(
                "Files must be 50 MB or smaller to keep device storage and memory use manageable. Share a smaller file.",
              );
            await holding.addBlob(
              new Blob([item.bytes.slice().buffer], { type: item.mime }),
              {
                ...base,
                kind: item.mime.startsWith("image/") ? "image" : "file",
              },
            );
          } else if (item.text?.trim()) {
            const size = new TextEncoder().encode(item.text).byteLength;
            if (size > MAX_SHARE_TEXT_BYTES)
              throw new Error(
                "Shared text must be 256 KB or smaller. Share longer text as a file to preserve draft storage on this device.",
              );
            holding.add({
              ...base,
              kind: isLink(item.text) ? "link" : "text",
              text: item.text,
              blobKey: null,
              size,
              mediaType: item.mime || "text/plain",
            });
          } else throw new Error("The shared item has no usable content.");
          added = true;
        } catch (error) {
          if (error instanceof InboxError && error.code === "storage")
            storageFailure = error;
          reportPlatformError({
            operation: `Share: ${item.name || "Shared item"}`,
            message: message(error),
          });
        }
      }
      await this.#move(profileId);
      if (added) announce("Added to Inbox");
      if (storageFailure) throw storageFailure;
    });
  }

  paired(profileId: string): Promise<void> {
    return this.#queue(() => this.#move(profileId)).catch((error: unknown) => {
      reportPlatformError({
        operation: "Inbox storage",
        message: message(error),
      });
    });
  }

  async #move(firstProfileId: string | null): Promise<void> {
    const holding = this.store(PHONE_INBOX);
    // Never remove the only saved copy after a storage failure.
    if (holding.get().error) holding.retrySave();
    for (const item of [...holding.get().items].reverse()) {
      const destination = item.destinationProfileId ?? firstProfileId;
      if (destination) holding.moveTo(item, this.store(destination));
    }
  }

  #queue(work: () => Promise<void>): Promise<void> {
    const result = this.#tail.then(work);
    this.#tail = result.catch(() => undefined);
    return result;
  }
}
function message(error: unknown): string {
  return error instanceof Error
    ? error.message
    : "The shared item couldn’t be saved on this device.";
}
function isLink(text: string): boolean {
  try {
    const url = new URL(text.trim());
    return (
      !/\s/.test(text.trim()) && ["https:", "http:"].includes(url.protocol)
    );
  } catch {
    return false;
  }
}
