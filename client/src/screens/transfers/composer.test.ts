// SPDX-License-Identifier: Apache-2.0
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TransferComposer } from "./composer";
import { transferName } from "./TransferRow";
beforeEach(() => localStorage.clear());

it("recovers a saved note when its filename changes after an initial read failure", () => {
  const original = new TransferComposer("read-failure");
  original.update({ mode: "text", text: "unsent note" });
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => {
    throw new Error("Storage temporarily unavailable");
  });
  const composer = new TransferComposer("read-failure");
  read.mockRestore();
  composer.update({ name: "renamed.md" });
  expect(new TransferComposer("read-failure").get()).toMatchObject({
    text: "unsent note", name: "renamed.md", mode: "text",
  });
});

it("protects an earlier note when a conflicting draft's recovery copy cannot be saved", () => {
  const original = new TransferComposer("conflict");
  original.update({ mode: "text", text: "earlier note" });
  const saved = localStorage.getItem("transfer-composer.conflict");
  const read = vi.spyOn(Storage.prototype, "getItem").mockImplementationOnce(() => {
    throw new Error("Unavailable");
  });
  const composer = new TransferComposer("conflict");
  read.mockRestore();
  const write = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Full"); });
  composer.update({ mode: "text", text: "new note" });
  expect(composer.get().error).toContain("could not be read or preserved");
  expect(composer.get().text).toBe("new note");
  expect(localStorage.getItem("transfer-composer.conflict")).toBe(saved);
  write.mockRestore();
  expect(composer.persistNow()).toBe(true);
  expect(JSON.parse(localStorage.getItem("transfer-composer.conflict.recovered")!)).toMatchObject({ text: "earlier note" });
  expect(new TransferComposer("conflict").get().text).toBe("new note");
});
describe("completed transfer drafts", () => {
  it.each(["submission", "retry"])("keeps a note recovered after a file upload starts, during %s", (recovery) => {
    new TransferComposer("recovered-upload").update({ mode: "text", text: "unsent note" });
    const read = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("Storage temporarily unavailable");
    });
    const composer = new TransferComposer("recovered-upload");
    composer.chooseFile({ fileName: "photo.jpg", fileSize: 42, fileId: "photo-choice" });
    // Transfers starts the upload from this in-memory file selection before
    // recording the job. Either that write or a later retry may restore text.
    expect(composer.get()).toMatchObject({ mode: "file", text: "", fileName: "photo.jpg" });
    if (recovery === "submission") read.mockRestore();
    composer.submitted("photo-job");
    read.mockRestore();
    expect(composer.persistNow()).toBe(true);
    expect(composer.get()).toMatchObject({ mode: "text", text: "unsent note", fileId: "photo-choice" });
    // Submission protection must survive an app restart too.
    const reopened = new TransferComposer("recovered-upload");
    expect(reopened.completed("photo-job")).toBe(false);
    expect(new TransferComposer("recovered-upload").get().text).toBe("unsent note");
  });
  it("clears a completed selection across app restarts so Send cannot repeat it", () => {
    const composer = new TransferComposer("phone");
    composer.update({ fileName: "guide.md", fileSize: 42, fileId: "chosen" });
    composer.submitted("job");
    const reopened = new TransferComposer("phone");
    expect(reopened.get().fileId).toBe("chosen");
    expect(reopened.completed("job")).toBe(true);
    expect(reopened.get()).toMatchObject({ fileName: "", fileId: undefined });
    expect(new TransferComposer("phone").get().fileName).toBe("");
    expect(reopened.completed("job")).toBe(false);
  });
  it("preserves the unsent draft in the other composer mode", () => {
    const composer = new TransferComposer("phone");
    composer.update({
      mode: "file",
      fileName: "file.md",
      fileSize: 5,
      text: "unsent text",
    });
    composer.submitted("file-job");
    expect(composer.completed("file-job")).toBe(true);
    expect(composer.get().text).toBe("unsent text");
    composer.update({ mode: "text", fileName: "another.md", fileSize: 9 });
    composer.submitted("text-job");
    composer.completed("text-job");
    expect(composer.get().fileName).toBe("another.md");
    expect(composer.get().text).toBe("");
  });
  it("keeps a newer selection when an earlier transfer finishes", () => {
    const composer = new TransferComposer("phone");
    composer.update({ mode: "text", text: "first" });
    composer.submitted("first-job");
    composer.update({ text: "new draft" });
    expect(composer.completed("first-job")).toBe(false);
    expect(composer.get().text).toBe("new draft");
  });
  it("clears submitted Text after a restart while keeping a file saved meanwhile", () => {
    const composer = new TransferComposer("phone");
    composer.update({ mode: "text", text: "sent text" });
    composer.submitted("text-job");
    composer.chooseFile({ fileName: "notes.txt", fileSize: 3, fileId: "saved" });
    const reopened = new TransferComposer("phone");
    expect(reopened.completed("text-job")).toBe(true);
    const saved = new TransferComposer("phone").get();
    expect(saved).toMatchObject({
      mode: "text", text: "",
      fileName: "notes.txt", fileSize: 3, fileId: "saved",
    });
    expect(saved.submitted).toBeUndefined();
  });
  it.each([
    { text: "new draft" },
    { name: "renamed.md" },
    { destination: { kind: "project_dir" as const, project_id: "project", dir: "notes" } },
  ])("preserves an edited Text draft when a file save finishes: %j", (edit) => {
    const composer = new TransferComposer("phone");
    composer.update({ mode: "text", text: "sent text" });
    composer.submitted("text-job");
    composer.update(edit);
    composer.chooseFile({ fileName: "notes.txt", fileSize: 3, fileId: "saved" });
    expect(composer.completed("text-job")).toBe(false);
    expect(composer.get()).toMatchObject({ ...edit, fileId: "saved" });
  });
  it("preserves File selected explicitly while a Text upload finishes", () => {
    const composer = new TransferComposer("phone");
    composer.update({ mode: "text", text: "sent text" });
    composer.submitted("text-job");
    composer.chooseFile({ mode: "file", fileName: "notes.txt", fileSize: 3, fileId: "saved" });
    expect(composer.completed("text-job")).toBe(false);
    expect(composer.get()).toMatchObject({ mode: "file", fileName: "notes.txt", fileId: "saved" });
  });
  it("preserves a same-name, same-size File replacement when an earlier upload finishes", () => {
    const composer = new TransferComposer("phone");
    composer.chooseFile({ fileName: "notes.txt", fileSize: 3, fileId: "first" });
    composer.submitted("file-job");
    composer.chooseFile({ fileName: "notes.txt", fileSize: 3, fileId: "replacement" });
    expect(composer.completed("file-job")).toBe(false);
    expect(composer.get()).toMatchObject({ fileName: "notes.txt", fileId: "replacement" });
  });
  it("keeps a draft saved for Attach to a message, destination and all, for the person to redirect", () => {
    localStorage.setItem(
      "transfer-composer.phone",
      JSON.stringify({
        mode: "file",
        text: "unsent text",
        name: "note.md",
        fileName: "report.pdf",
        fileSize: 42,
        destination: { kind: "artifact" },
        error: null,
      }),
    );
    expect(new TransferComposer("phone").get()).toMatchObject({
      text: "unsent text",
      fileName: "report.pdf",
      fileSize: 42,
      destination: { kind: "artifact" },
    });
  });
  it("uses the destination name when a history record has only an opaque identity", () => {
    expect(
      transferName(
        "0192f7a1-0000-7000-8000-000000000201",
        "C:\\Inbox\\report.md",
      ),
    ).toBe("report.md");
    expect(transferName(undefined)).toBe("File transfer");
    expect(
      transferName(undefined, "/Inbox/0192f7a1-0000-7000-8000-000000000201"),
    ).toBe("File transfer");
    expect(transferName("notes.md", "/inbox/notes (1).md")).toBe("notes.md");
  });
});
