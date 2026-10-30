// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it } from "vitest";
import type { ControlTarget } from "../protocol/types";
import { emptyHostState } from "../state/projection";
import { prefs } from "./prefs";
import { HostStateStore, nameScreens, renameScreen } from "./screenNames";

const target = (target_id: string, kind: ControlTarget["kind"], native_ref: string, label = native_ref) =>
  ({ target_id, kind, native_ref, label }) as ControlTarget;
const dp = target("a", "output", "DP-1");
const hdmi = target("b", "output", "HDMI-A-1");
const win = target("c", "window", "0x2a", "Firefox — Staging dashboard");
const whole = target("d", "desktop", "all", "Entire desktop");

afterEach(() => prefs.set((p) => ({ ...p, screenNames: {} })));

it("numbers screens in the computer's order and keeps the connector", () => {
  const named = nameScreens([whole, dp, win, hdmi, target("e", "output", "\\\\.\\DISPLAY3")], () => undefined);
  expect(named.map((t) => t.label)).toEqual(["Entire desktop", "Screen 1 (DP-1)", "Firefox — Staging dashboard", "Screen 2 (HDMI-A-1)", "Screen 3 (DISPLAY3)"]);
  expect(nameScreens(named, () => undefined)).toBe(named);
});

it("keeps a name given on this device per computer, and goes back to the number when cleared", () => {
  const store = new HostStateStore(emptyHostState, () => "home");
  store.set((s) => ({ ...s, targets: [dp, hdmi] }));
  expect(store.get().targets.map((t) => t.label)).toEqual(["Screen 1 (DP-1)", "Screen 2 (HDMI-A-1)"]);

  renameScreen("home", hdmi, "  Left monitor  ");
  store.renamed();
  expect(store.get().targets.map((t) => t.label)).toEqual(["Screen 1 (DP-1)", "Left monitor"]);
  expect(JSON.parse(localStorage.getItem("prefs") ?? "{}").screenNames).toEqual({ "home HDMI-A-1": "Left monitor" });

  // A new report from the computer keeps the name; another computer's screens don't take it.
  store.set((s) => ({ ...s, targets: [hdmi, dp] }));
  expect(store.get().targets.map((t) => t.label)).toEqual(["Left monitor", "Screen 2 (DP-1)"]);
  const other = new HostStateStore(emptyHostState, () => "office");
  other.set((s) => ({ ...s, targets: [dp, hdmi] }));
  expect(other.get().targets.map((t) => t.label)).toEqual(["Screen 1 (DP-1)", "Screen 2 (HDMI-A-1)"]);

  renameScreen("home", hdmi, " ");
  store.renamed();
  expect(store.get().targets.map((t) => t.label)).toEqual(["Screen 1 (HDMI-A-1)", "Screen 2 (DP-1)"]);
  expect(prefs.get().screenNames).toEqual({});
});
