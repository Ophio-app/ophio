// SPDX-License-Identifier: Apache-2.0
/** Original and partial bytes stay on this phone until explicitly removed. */
async function db(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open("transfer-bytes", 1);
    request.onupgradeneeded = () => request.result.createObjectStore("files");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () =>
      reject(new Error("Couldn’t open this device’s file storage."));
  });
}
async function transaction<T>(
  mode: IDBTransactionMode,
  fn: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const database = await db();
  return new Promise((resolve, reject) => {
    const tx = database.transaction("files", mode),
      request = fn(tx.objectStore("files"));
    tx.oncomplete = () => {
      database.close();
      resolve(request.result);
    };
    tx.onerror = tx.onabort = () => {
      database.close();
      reject(new Error("This device’s file storage is full or unavailable."));
    };
  });
}
export const transferBytes = {
  put: (key: string, blob: Blob) =>
    transaction("readwrite", (store) => store.put(blob, key)).then(
      () => undefined,
    ),
  get: (key: string): Promise<Blob | null> =>
    transaction("readonly", (store) => store.get(key)).then((v: unknown) =>
      v instanceof Blob ? v : null,
    ),
  remove: (key: string) =>
    transaction("readwrite", (store) => store.delete(key)).then(
      () => undefined,
    ),
  /** Copy bytes to a new pairing; retain originals if another store cannot move. */
  move: (from: string, to: string) =>
    transaction("readwrite", (store) => {
      const request = store.openCursor(
        IDBKeyRange.bound(`${from}:`, `${from}:￿`),
      );
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return;
        const next = `${to}${String(cursor.key).slice(from.length)}`;
        const existing = store.getKey(next);
        existing.onsuccess = () => {
          if (existing.result === undefined) store.put(cursor.value, next);
          cursor.continue();
        };
      };
      return request;
    }).then(() => undefined),
};
