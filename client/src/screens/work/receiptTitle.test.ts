// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { TimelineItem } from "../../protocol/types";
import { receiptTitle } from "./receiptTitle";

const asked = (...texts: string[]): TimelineItem[] =>
  texts.map((text, i) => ({ seq: i + 1, run_id: null, at: i, item: { kind: "user_message", text, bundle: null, delivery: "native_delivered" } }));
const request = "Work out how to add request tracing headers across the payments client and its retry path. Keep the existing log format.";

it("finishes the sentence a cut title left off", () => {
  expect(receiptTitle("Work out how to add request tracing headers across the payments clien…", asked(request, "Also the docs")))
    .toBe("Work out how to add request tracing headers across the payments client and its retry path.");
  // The computer cuts an untitled task at 80 characters without marking it.
  expect(receiptTitle(request.slice(0, 80), asked(request))).toBe("Work out how to add request tracing headers across the payments client and its retry path.");
});

it("keeps a long sentence to a readable heading at a word boundary", () => {
  const long = `${"Trace every outgoing request ".repeat(8).trim()}.`;
  const shown = receiptTitle(`${long.slice(0, 69)}…`, asked(long));
  expect(shown.length).toBeLessThanOrEqual(120);
  expect(shown).toMatch(/request…$/);
});

it("shows the title as written when it wasn't cut from the loaded request", () => {
  expect(receiptTitle("Tracing headers", asked(request))).toBe("Tracing headers");
  expect(receiptTitle("Work out how to add request tracing headers across the payments clien…", asked("Something else"))).toBe("Work out how to add request tracing headers across the payments clien…");
  expect(receiptTitle("Work out how to add request tracing headers across the payments clien…", [])).toBe("Work out how to add request tracing headers across the payments clien…");
});
