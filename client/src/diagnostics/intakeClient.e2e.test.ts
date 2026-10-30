// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import type { ReportBundle, ReportProgress, ReportQuestion } from "../protocol/types";
import { IntakeClient, intakePaths, signingHeaders } from "./intakeClient";
import { ReportService } from "./reports";
import { PhoneTrail } from "./trail";
import { CrashRegistry } from "./crashes";

declare const process: { env: Record<string, string | undefined> };
const endpoint = process.env.OPHIO_INTAKE_E2E_URL;
const maintainer = process.env.OPHIO_INTAKE_E2E_MAINTAINER_URL;
const token = process.env.OPHIO_INTAKE_E2E_TOKEN;
type IntakeRecord = { bundle?: ReportBundle; questions: ReportQuestion[]; history: { status: ReportProgress }[] };
async function admin<T>(path: string, value?: unknown): Promise<T> {
  const response = await fetch(maintainer + "/maintainer/v1/" + path, {
    method: value === undefined ? "GET" : "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    ...(value === undefined ? {} : { body: JSON.stringify(value) }), redirect: "error",
  });
  expect(response.status).toBe(200);
  return response.json() as Promise<T>;
}

describe.skipIf(!endpoint)("phone direct reporting against the real intake", () => {
  it("submits a reviewed bundle, reads status, answers a question, and withdraws", async () => {
    expect(endpoint).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    expect(maintainer).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    expect(token).toBeTruthy();
    const pair = await crypto.subtle.generateKey("Ed25519", false, ["sign", "verify"]) as CryptoKeyPair;
    expect(pair.privateKey.extractable).toBe(false);
    const client = new IntakeClient(endpoint, { get: async () => pair });
    expect(await client.available()).toBe(true);
    localStorage.clear();
    const service = new ReportService({ intake: client, trail: new PhoneTrail(), crashes: new CrashRegistry() });
    const draft = await service.startReport({ type: "error" }, {
      screen: "/settings", task: "01900000-0000-7000-8000-000000000001",
      error: { code: "empty", message: "Could not read /workspace/private at https://private.example with 'private name'" },
    });
    service.setWords(draft, "The screen stayed empty");
    expect(service.send(draft)).toBe(draft.reference);
    await service.flush();
    expect(service.get().queued).toEqual([]);
    expect(service.get().direct[0]).toMatchObject({ report_id: draft.id, reference: draft.reference, history: [{ status: { state: "received" } }] });
    const record = await admin<IntakeRecord>(`reports/${draft.id}`);
    expect(record.bundle).toEqual(JSON.parse(JSON.stringify(draft.bundle)));
    const serialized = JSON.stringify(record.bundle);
    for (const privateValue of [draft.salt, "/workspace/private", "https://private.example", "private name", "01900000-0000-7000-8000-000000000001"]) expect(serialized).not.toContain(privateValue);
    expect(serialized).toContain("<path>");
    const receipt = await client.submit(draft.bundle);
    expect(receipt).toMatchObject({ report_id: draft.id, reference: draft.reference });
    expect((await admin<IntakeRecord>(`reports/${draft.id}`)).history).toEqual(record.history);
    await admin("command", { action: "set_status", report_id: draft.id, status: { state: "reproduced" } });
    await service.poll();
    expect(service.get().direct[0]?.history.at(-1)?.status).toEqual({ state: "reproduced" });
    const { question } = await admin<{ question: ReportQuestion }>("command", { action: "ask", report_id: draft.id, template: { template: "happened_again" } });
    await service.poll();
    expect(service.get().direct[0]?.questions).toEqual([question]);
    await service.answer({ report_id: draft.id, question_id: question.question_id, answer: { choice: { choice: "yes" }, words: "It happened again", at: Date.now() } });
    expect((await admin<IntakeRecord>(`reports/${draft.id}`)).questions[0]?.answer).toMatchObject({ choice: { choice: "yes" }, words: "It happened again" });
    expect(service.get().direct[0]?.questions.every(q => !!q.answer)).toBe(true);
    await service.withdraw(draft.id);
    expect((await admin<IntakeRecord>(`reports/${draft.id}`)).bundle).toBeNull();
    expect(service.get().direct[0]?.history.at(-1)?.status).toEqual({ state: "withdrawn" });
    expect((await client.status({ report_ids: [draft.id] })).reports[0]?.questions).toEqual([]);
    expect(await client.releases()).toMatchObject({ releases: [] });
  });

  it("allows the phone webview's cross-origin preflight on every reporter path", async () => {
    expect(endpoint).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
    for (const path of Object.values(intakePaths)) {
      const response = await fetch(endpoint + path, { method: "OPTIONS", headers: {
        origin: "http://tauri.localhost", "access-control-request-method": path === intakePaths.releases ? "GET" : "POST",
        "access-control-request-headers": ["content-type", ...Object.values(signingHeaders)].join(", "),
      } });
      expect(response.status).toBe(204);
      expect(response.headers.get("access-control-allow-origin")).toBe("*");
      expect(response.headers.get("access-control-allow-credentials")).toBeNull();
      for (const header of ["content-type", ...Object.values(signingHeaders)]) expect(response.headers.get("access-control-allow-headers")?.toLowerCase()).toContain(header);
      expect(response.headers.get("access-control-expose-headers")?.toLowerCase()).toContain("retry-after");
    }
  });
});
