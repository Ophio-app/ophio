// SPDX-License-Identifier: Apache-2.0
import type { CrashRecord, ReportExtra, ReportOrigin } from "../protocol/types";
import type { ReportContextInput } from "../diagnostics/bundle";
import { Store } from "../state/store";

export interface ReportRequest { origin: ReportOrigin; context: ReportContextInput; crashes?: CrashRecord[]; extra?: ReportExtra; onSent?: () => void }
export const reportRequest = new Store<ReportRequest | null>(null);
let context = (): ReportContextInput => ({ screen: "/welcome" });
export function reportContext(get: () => ReportContextInput): () => void {
  context = get;
  return () => { if (context === get) context = () => ({ screen: "/welcome" }); };
}
export function openReport(origin: ReportOrigin = { type: "settings" }, details: Partial<ReportContextInput> = {}, rest: Pick<ReportRequest, "crashes" | "extra" | "onSent"> = {}): void {
  reportRequest.set({ origin, context: { ...context(), ...details }, ...rest });
}
