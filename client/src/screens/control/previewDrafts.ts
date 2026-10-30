// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { uuidv7 } from "../../protocol/ids";
export interface PreviewDraft {
  id: string;
  profileId: string;
  previewId: string;
  previewLabel?: string;
  computerId?: string;
  computerName?: string;
  projectId: string;
  taskId: string | null;
  blob: Blob;
  at: number;
  captureTimeKnown?: boolean;
  transferId?: string;
  artifactId?: string;
}
async function transaction<T>(
  mode: IDBTransactionMode,
  op: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open("preview-pictures", 1);
    request.onupgradeneeded = () =>
      request.result.createObjectStore("drafts", { keyPath: "id" });
    request.onsuccess = () => resolve(request.result);
    request.onerror = () =>
      reject(new Error("Preview picture storage could not open."));
  });
  return new Promise((resolve, reject) => {
    const tx = db.transaction("drafts", mode),
      request = op(tx.objectStore("drafts"));
    tx.oncomplete = () => {
      db.close();
      resolve(request.result);
    };
    tx.onerror = tx.onabort = () => {
      db.close();
      reject(new Error("The preview picture was not saved on this device."));
    };
  });
}
export const previewDrafts = {
  id: () => uuidv7(Date.now()),
  list: async (profileId: string, previewId: string): Promise<PreviewDraft[]> =>
    (await transaction<PreviewDraft[]>("readonly", (s) => s.getAll())).filter(
      (d) => d.profileId === profileId && d.previewId === previewId,
    ),
  put: async (draft: PreviewDraft): Promise<void> => {
    await transaction("readwrite", (s) => s.put(draft));
  },
  remove: async (id: string): Promise<void> => {
    await transaction("readwrite", (s) => s.delete(id));
  },
  /** Give one computer's pictures to its new pairing. */
  move: async (from: string, to: string): Promise<void> => {
    await transaction("readwrite", (s) => {
      const request = s.openCursor();
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        const draft = cursor.value as PreviewDraft;
        if (draft.profileId === from) cursor.update({ ...draft, profileId: to });
        cursor.continue();
      };
      return request;
    });
  },
};
export const previewCloseText = (reason: string): string =>
  ({
    closed: "Preview closed",
    removed: "Closed by the computer: the preview was removed.",
    device_revoked: "Closed by the computer: this device’s access was revoked.",
    idle_timeout:
      "Preview closed after being idle. Open it again to reconnect.",
    control_lost: "Preview connection lost. Open it again after reconnecting.",
    lost_connection:
      "Preview connection lost. Open it again after reconnecting.",
    host_shutdown: `Closed by the computer: ${PRODUCT_NAME} stopped.`,
    server_closed: "Closed by the computer: the preview server stopped.",
  })[reason] ?? `Closed by the computer: ${reason}`;
