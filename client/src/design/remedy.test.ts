// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { commandIn, remedyText } from "./remedy";

it("says where a bare command for the computer runs and leaves sentences alone", () => {
  expect(remedyText("ophio voice install tiny.en")).toBe(
    "On the computer, run: ophio voice install tiny.en",
  );
  expect(remedyText("Open Hermes on the computer and sign in.")).toBe(
    "Open Hermes on the computer and sign in.",
  );
});

it("lifts a computer command out of a remedy so it can be copied", () => {
  expect(commandIn("ophio channels token set discord")).toEqual({
    lead: "On the computer, run the command below.", command: "ophio channels token set discord",
  });
  expect(commandIn("Needs setup. On the computer, run: ophio voice install tiny.en.")).toEqual({
    lead: "Needs setup. On the computer, run the command below.", command: "ophio voice install tiny.en",
  });
  expect(commandIn("Run ophio desktop enable on the computer to permit desktop capture and input")).toEqual({
    lead: "On the computer, run the command below to permit desktop capture and input.", command: "ophio desktop enable",
  });
  expect(commandIn("Open Hermes on the computer and sign in.")).toBeNull();
});
