// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { SourceDraft } from "../../state/annotations";
import { originWords } from "./sources";

const source = (original_name: string | null) => ({ origin: { kind: "phone_import", original_name }, derivation: null }) as unknown as SourceDraft;

it("names a retained computer preview instead of calling it a device picture", () => {
  expect(originWords(source("Preview: Garden · 2026-09-28T08:00:00.000Z.png"))).toBe("From preview · Garden");
  expect(originWords(source("Preview: Web preview · 2026-09-28T08:00:00.000Z.png"))).toBe("From preview · Web preview");
  expect(originWords(source("holiday.png"))).toBe("From this device");
  expect(originWords(source(null))).toBe("From this device");
});

it("does not invent a capture time for a recovered native preview", () => {
  const words = originWords({ ...source(null), capturedAt: Date.now(), origin: {
    kind: "preview_capture", capture_id: "picture", host_id: "host", computer_name: "studio",
    preview_id: "preview", preview_label: "Notes", captured_at: null,
  } });
  expect(words).toBe("Taken from the Notes preview on studio · Capture time was not recorded");
});
