// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { projectFileLink } from "./fileLink";

describe("projectFileLink", () => {
  const root = { host_path: "/home/sam/site" };
  it("opens a file in the project's folder, by its path there or on the computer", () => {
    expect(projectFileLink("docs/report.md", root)).toBe("docs/report.md");
    expect(projectFileLink("./docs/My%20notes.md#top", root)).toBe("docs/My notes.md");
    expect(projectFileLink("/home/sam/site/out/shot.png", root)).toBe("out/shot.png");
    expect(projectFileLink("file:///home/sam/site/README.md", root)).toBe("README.md");
  });
  it("opens the file a cited line is in", () => {
    expect(projectFileLink("/home/sam/site/docs/report.md:12", root)).toBe("docs/report.md");
    expect(projectFileLink("docs/report.md:12", root)).toBe("docs/report.md");
    expect(projectFileLink("docs/report.md:12:5", root)).toBe("docs/report.md");
    expect(projectFileLink("docs/report.md:12-20", root)).toBe("docs/report.md");
    expect(projectFileLink("README.md:3", root)).toBe("README.md");
    expect(projectFileLink("./Makefile:7", root)).toBe("Makefile");
    expect(projectFileLink("file:///home/sam/site/README.md:3", root)).toBe("README.md");
    expect(projectFileLink("docs/report.md#L12", root)).toBe("docs/report.md");
  });
  it("opens a file by its Windows path, with or without a line", () => {
    const windows = { host_path: "C:\\Users\\sam\\site" };
    expect(projectFileLink("C:\\Users\\sam\\site\\docs\\report.md", windows)).toBe("docs/report.md");
    expect(projectFileLink("C:\\Users\\sam\\site\\docs\\report.md:12", windows)).toBe("docs/report.md");
    expect(projectFileLink("C:/Users/sam/site/docs/report.md:12:5", windows)).toBe("docs/report.md");
    expect(projectFileLink("C:\\Users\\sam\\other\\report.md:12", windows)).toBeNull();
  });
  it("leaves web addresses, anchors and anywhere else alone", () => {
    expect(projectFileLink("https://example.com/a.md", root)).toBeNull();
    expect(projectFileLink("https://example.com/a.md:12", root)).toBeNull();
    expect(projectFileLink("http://localhost:3000", root)).toBeNull();
    expect(projectFileLink("localhost:3000", root)).toBeNull();
    expect(projectFileLink("tel:5551234", root)).toBeNull();
    expect(projectFileLink("mailto:a@b.c", root)).toBeNull();
    expect(projectFileLink("#section", root)).toBeNull();
    expect(projectFileLink("/home/sam/other/a.md", root)).toBeNull();
    expect(projectFileLink("/home/sam/other/a.md:12", root)).toBeNull();
    expect(projectFileLink("/home/sam/sitex/a.md", root)).toBeNull();
    expect(projectFileLink("/home/sam/site:12", root)).toBeNull();
    expect(projectFileLink("../secrets.txt", root)).toBeNull();
    expect(projectFileLink("../secrets.txt:3", root)).toBeNull();
    expect(projectFileLink("docs/report.md", undefined)).toBeNull();
  });
});
