// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { numberedPath, UploadStaging } from "../../dev/sim/transfers";
import { relativeDir } from "../library/model";
import { saveToPhone } from "./saveToPhone";
import { simulatedDevice } from "../../dev/sim/device";

describe("transfer safeguards", () => {
  it("keeps a numbered sibling beside colliding files", () => {
    expect(
      numberedPath(
        "/Inbox/report.md",
        new Set(["/Inbox/report.md", "/Inbox/report (1).md"]),
      ),
    ).toBe("/Inbox/report (2).md");
    expect(numberedPath("/Inbox/.notes", new Set(["/Inbox/.notes"]))).toBe(
      "/Inbox/.notes (1)",
    );
  });
  it("retains verified upload offsets across simulated transfer connections", () => {
    const s = new UploadStaging();
    const first = s.open("t1");
    first.parts.push(new Uint8Array(100));
    first.received = 100;
    expect(s.open("t1").received).toBe(100);
    s.remove("t1");
    expect(s.open("t1").received).toBe(0);
  });
  it("rejects project destination traversal on both supported path syntaxes", () => {
    expect(relativeDir("..\\secret")).not.toBeNull();
    expect(relativeDir("notes/../../secret")).not.toBeNull();
    expect(relativeDir("D:/files")).not.toBeNull();
  });
  it("does not claim a save without a platform implementation", async () => {
    await expect(saveToPhone(new Blob(["x"]), "x.md", {})).rejects.toThrow(
      "isn’t available",
    );
  });
  it("reports where this device saved the file, numbering a repeated name", async () => {
    const { capabilities } = simulatedDevice();
    await expect(
      saveToPhone(new Blob(["x"]), "metrics.csv", capabilities),
    ).resolves.toBe("Downloads/metrics.csv");
    await expect(
      saveToPhone(new Blob(["x"]), "metrics.csv", capabilities),
    ).resolves.toBe("Downloads/metrics (1).csv");
  });
});
