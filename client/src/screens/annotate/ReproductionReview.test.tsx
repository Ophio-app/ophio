// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { annotationStore } from "../../state/annotations";
import { ReproductionReview } from "./ReproductionReview";
vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: "data:image/png;base64,", error: null }) }));
afterEach(cleanup);
it("renders each distinct reviewed range once above the ordered steps", () => {
  const draft = annotationStore("trim-review-test").create("project", null);
  draft.steps = [
    { index: 1, source_key: "S1", text: "Open menu", clip_range_ms: [300, 1100] },
    { index: 2, source_key: "S2", text: "Chart disappears", clip_range_ms: [300, 1100] },
  ];
  const { container } = render(<ReproductionReview draft={draft} />);
  expect(screen.getAllByText("Clip range: 0.30–1.10 s")).toHaveLength(1);
  expect(container.querySelector(".an-outgoing-steps")?.textContent).not.toContain("range:");
  expect(container.textContent).toContain("this device");
});
