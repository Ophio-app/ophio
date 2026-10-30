// SPDX-License-Identifier: Apache-2.0
import type { IntakeAnswer, IntakeReceipt, IntakeSubmission, ReleaseFeed, StatusRequest, StatusResponse, WithdrawRequest } from "../protocol/types";
import { hex } from "./pseudonym";

export const intakePaths = { submit: "/v1/reports", status: "/v1/reports/status", answer: "/v1/reports/answer", withdraw: "/v1/reports/withdraw", releases: "/v1/releases" } as const;
export const signingHeaders = { key: "ophio-report-key", time: "ophio-report-time", signature: "ophio-report-signature" } as const;
const encoder = new TextEncoder();
export async function signingPayload(method: string, path: string, time: number, body: string): Promise<Uint8Array<ArrayBuffer>> {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(body));
  return encoder.encode(`ophio-report-v1\n${method.toUpperCase()}\n${path}\n${time}\n${hex(new Uint8Array(digest))}`);
}
export function base64url(bytes: Uint8Array): string {
  return btoa(Array.from(bytes, b => String.fromCharCode(b)).join("")).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
export interface ReporterKeys { get(): Promise<CryptoKeyPair | null> }
/** IndexedDB structured-clones CryptoKeys without exporting the private key. */
export class IndexedReporterKeys implements ReporterKeys {
  #pending: Promise<CryptoKeyPair | null> | undefined;
  get(): Promise<CryptoKeyPair | null> { return this.#pending ??= this.#load().catch(() => null); }
  async #load(): Promise<CryptoKeyPair | null> {
    if (!globalThis.indexedDB || !globalThis.crypto?.subtle) return null;
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("ophio-reporter", 1);
      request.onupgradeneeded = () => request.result.createObjectStore("keys");
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
      request.onblocked = () => reject(new Error("Reporter storage is unavailable"));
    });
    try {
      const read = () => new Promise<CryptoKeyPair | undefined>((resolve, reject) => {
        const transaction = db.transaction("keys", "readonly");
        const request = transaction.objectStore("keys").get("reporter");
        transaction.oncomplete = () => resolve(request.result as CryptoKeyPair | undefined);
        transaction.onabort = () => reject(transaction.error);
      });
      const existing = await read();
      if (existing) return existing;
      const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]) as CryptoKeyPair;
      // Another tab may have made the key while WebCrypto was working.
      return await new Promise<CryptoKeyPair>((resolve, reject) => {
        const transaction = db.transaction("keys", "readwrite");
        const store = transaction.objectStore("keys");
        const request = store.get("reporter");
        let chosen = pair;
        request.onsuccess = () => { if (request.result) chosen = request.result as CryptoKeyPair; else store.put(pair, "reporter"); };
        transaction.oncomplete = () => resolve(chosen);
        transaction.onabort = () => reject(transaction.error);
      });
    } finally { db.close(); }
  }
}
const reporterKeys = new IndexedReporterKeys();
export class IntakeUnavailable extends Error {
  constructor() { super("Direct reporting isn’t available on this device."); }
}
export class IntakeClient {
  readonly baseUrl: string;
  constructor(baseUrl = import.meta.env.VITE_OPHIO_INTAKE_URL ?? "", readonly keys: ReporterKeys = reporterKeys, readonly fetcher: typeof fetch = (...args) => fetch(...args), readonly now: () => number = Date.now) {
    const value = baseUrl.trim().replace(/\/$/, "");
    // Development fakes may use loopback HTTP; installed endpoints use HTTPS.
    try { const url = new URL(value); this.baseUrl = !url.username && !url.password && !url.search && !url.hash && (url.protocol === "https:" || url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)) ? value : ""; }
    catch { this.baseUrl = ""; }
  }
  async available(): Promise<boolean> { return !!this.baseUrl && !!await this.keys.get(); }
  async #request<T>(method: "GET" | "POST", path: string, value?: unknown): Promise<T> {
    const keys = this.baseUrl ? await this.keys.get() : null;
    if (!keys) throw new IntakeUnavailable();
    const body = value === undefined ? "" : JSON.stringify(value);
    const time = this.now();
    // Sign the path actually requested, including a configured base prefix.
    const target = new URL(this.baseUrl + path);
    const payload = await signingPayload(method, target.pathname, time, body);
    const signature = await crypto.subtle.sign("Ed25519", keys.privateKey, payload);
    const publicKey = await crypto.subtle.exportKey("raw", keys.publicKey);
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 20_000);
    try {
      const response = await this.fetcher(target.href, { method, ...(method === "POST" ? { body } : {}), credentials: "omit", redirect: "error", cache: "no-store", signal: controller.signal,
        headers: { "content-type": "application/json", [signingHeaders.key]: base64url(new Uint8Array(publicKey)), [signingHeaders.time]: String(time), [signingHeaders.signature]: base64url(new Uint8Array(signature)) } });
      if (!response.ok) throw new Error("The report service couldn’t answer. It will be tried again.");
      return await response.json() as T;
    } finally { clearTimeout(timeout); }
  }
  submit(bundle: IntakeSubmission["bundle"]): Promise<IntakeReceipt> { return this.#request("POST", intakePaths.submit, { bundle, sender: "phone" } satisfies IntakeSubmission); }
  status(value: StatusRequest): Promise<StatusResponse> { return this.#request("POST", intakePaths.status, value); }
  answer(value: IntakeAnswer): Promise<StatusResponse> { return this.#request("POST", intakePaths.answer, value); }
  withdraw(value: WithdrawRequest): Promise<StatusResponse> { return this.#request("POST", intakePaths.withdraw, value); }
  releases(): Promise<ReleaseFeed> { return this.#request("GET", intakePaths.releases); }
}
