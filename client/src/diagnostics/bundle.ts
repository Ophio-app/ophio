// SPDX-License-Identifier: Apache-2.0
import type { ComputerPart, CrashRecord, PhoneVersions, ReportBundle, ReportContext, ReportExtraPart, ReportOrigin, ReportScreenshot, ReportSide, TrailEvent } from "../protocol/types";
import { uuidv7 } from "../protocol/ids";
import { MAX_REPORT_BYTES, MAX_REPORT_CODE, MAX_REPORT_WORDS, MAX_SCREENSHOT_BYTES, REPORT_SCHEMA } from "./constants";
import { pseudonym, type PseudonymKind } from "./pseudonym";
import { routePattern } from "./routes";
import { scrub } from "./scrub";
import { stateShape } from "./stateShape";

export const cutWords = (words: string) => Array.from(words).slice(0, MAX_REPORT_WORDS).join("");
const cutCode = (code: string) => Array.from(code).slice(0, MAX_REPORT_CODE).join("");
export const bundleBytes = (bundle: ReportBundle) => new TextEncoder().encode(JSON.stringify(bundle)).byteLength;

export async function redactTrail(events: TrailEvent[], salt: string): Promise<TrailEvent[]> {
  return Promise.all(events.map(async ({ at, event }) => {
    const copy = { ...event };
    for (const field of ["task", "run", "approval"] as const) if (field in copy) {
      const record = copy as unknown as Record<string, string>;
      record[field] = await pseudonym(salt, field as PseudonymKind, record[field]!);
    }
    if (copy.type === "route") copy.route = routePattern(copy.route);
    if (copy.type === "error") copy.error = { code: cutCode(copy.error.code), message: scrub(copy.error.message ?? "") };
    return { at, event: copy };
  }));
}
export function checkedScreenshot(value: ReportScreenshot | undefined): ReportScreenshot | undefined {
  if (!value) return undefined;
  if (!["image/png", "image/jpeg", "image/webp"].includes(value.media_type)
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value.data)
    || value.data.length / 4 * 3 - (value.data.endsWith("==") ? 2 : value.data.endsWith("=") ? 1 : 0) > MAX_SCREENSHOT_BYTES
    || !Number.isSafeInteger(value.width) || value.width <= 0 || !Number.isSafeInteger(value.height) || value.height <= 0) throw new Error("This picture couldn’t be included.");
  return structuredClone(value);
}

export type ReportContextInput = ReportContext & { errorSide?: ReportSide };
export interface BuildReport {
  id?: string;
  now?: number;
  salt: string;
  origin: ReportOrigin;
  context: ReportContextInput;
  words?: string;
  phone?: PhoneVersions;
  trail?: TrailEvent[];
  store?: unknown;
  crashes?: CrashRecord[];
  screenshot?: ReportScreenshot;
  extras?: ReportExtraPart[];
  computer?: ComputerPart;
}
export async function buildBundle(input: BuildReport): Promise<ReportBundle> {
  const context: ReportContext = {
    screen: routePattern(input.context.screen),
    ...(input.context.task ? { task: await pseudonym(input.salt, "task", input.context.task) } : {}),
    ...(input.context.harness ? { harness: input.context.harness } : {}),
    ...(input.context.model ? { model: cutCode(input.context.model) } : {}),
    ...(input.context.command ? { command: cutCode(input.context.command) } : {}),
    ...(input.context.error ? { error: { code: cutCode(input.context.error.code), message: scrub(input.context.error.message ?? "") } } : {}),
  };
  const bundle: ReportBundle = {
    schema: REPORT_SCHEMA, report_id: input.id ?? uuidv7(), created_at: input.now ?? Date.now(), origin: input.origin,
    ...(input.words ? { words: cutWords(input.words) } : {}),
    signature: { code: context.error?.code ?? (input.origin.type === "crash" ? input.crashes?.[0]?.code : undefined) ?? "none", screen: context.screen, build: (input.origin.type === "crash" ? input.crashes?.[0]?.build : undefined) ?? input.phone?.build ?? "unknown" },
    versions: input.phone ? { phone: input.phone } : {}, context,
    phone_trail: await redactTrail(input.trail ?? [], input.salt), computer_trail: [],
    ...(input.store !== undefined ? { state: await stateShape(input.store, input.salt) } : {}),
    crashes: structuredClone(input.crashes ?? []),
    ...(input.screenshot ? { screenshot: checkedScreenshot(input.screenshot) } : {}), extras: structuredClone(input.extras ?? []),
  };
  return enforceSize(input.computer ? mergeComputer(bundle, input.computer, input.context.errorSide ?? (context.command && context.error ? "computer" : "phone")) : bundle);
}

/** The preview has already been redacted by the computer, using this draft's salt. */
export function mergeComputer(bundle: ReportBundle, computer: ComputerPart, signatureSide: ReportSide = "phone"): ReportBundle {
  return { ...bundle, signature: signatureSide === "computer" ? { ...bundle.signature, build: computer.versions.build } : bundle.signature, versions: { ...bundle.versions, computer: structuredClone(computer.versions) }, computer_trail: structuredClone(computer.trail),
    crashes: [...bundle.crashes.filter(c => c.side === "phone"), ...structuredClone(computer.crashes)].sort((a, b) => b.at - a.at), extras: structuredClone(computer.extras) };
}

/** Stable loss order: store, oldest events across both trails, picture, extras,
 * then oldest crashes. Required fields and the person's words are never cut. */
export function enforceSize(input: ReportBundle, limit = MAX_REPORT_BYTES): ReportBundle {
  const bundle = structuredClone(input);
  const fits = () => bundleBytes(bundle) <= limit;
  if (fits()) return bundle;
  delete bundle.state;
  while (!fits() && (bundle.phone_trail.length || bundle.computer_trail.length)) {
    const phone = bundle.phone_trail[0]?.at ?? Infinity;
    const computer = bundle.computer_trail[0]?.at ?? Infinity;
    (phone <= computer ? bundle.phone_trail : bundle.computer_trail).shift();
  }
  if (fits()) return bundle;
  delete bundle.screenshot;
  while (!fits() && bundle.extras.length) {
    const first = bundle.extras[0]!;
    if (first.type === "task_transcript" && first.entries.length) { first.entries.shift(); first.cut = true; }
    else if (first.type === "names" && first.names.length) first.names.shift();
    else bundle.extras.shift();
  }
  while (!fits() && bundle.crashes.length) bundle.crashes.pop();
  if (!fits()) throw new Error("This report is too large to send.");
  return bundle;
}

export interface BundleField { path: (string | number)[]; label: string; value: unknown }
export interface BundleSection { label: string; fields: BundleField[] }
const labels: Record<keyof ReportBundle, string> = {
  schema: "Report format", report_id: "Report ID", created_at: "Created at", origin: "How it started", words: "Your words", signature: "Problem signature",
  versions: "Versions", context: "Where it happened", phone_trail: "What the phone did", computer_trail: "What the computer did", state: "Screen state", crashes: "Crashes", screenshot: "Picture", extras: "What you added", test: "Your test and suggested fix",
};
/** Every leaf, including empty containers, is present with its exact wire value.
 * Paths let a sheet render rich labels without hiding unknown future fields. */
export function describeBundle(bundle: ReportBundle): BundleSection[] {
  function fields(value: unknown, path: (string | number)[]): BundleField[] {
    if (value !== null && typeof value === "object" && Object.keys(value).length) {
      return Object.entries(value).flatMap(([k, v]) => fields(v, [...path, Array.isArray(value) ? Number(k) : k]));
    }
    return [{ path, label: String(path.at(-1)).replace(/_/g, " "), value }];
  }
  // JSON is the submission boundary: omitted undefined fields are omitted here too.
  const sent = JSON.parse(JSON.stringify(bundle)) as ReportBundle;
  return Object.entries(sent).map(([key, value]) => ({ label: labels[key as keyof ReportBundle] ?? key.replace(/_/g, " "), fields: fields(value, [key]) }));
}
