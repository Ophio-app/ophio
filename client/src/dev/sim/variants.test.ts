// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it } from "vitest";
import { SimHost } from "./host";
import { capabilities } from "./scenario";
import { hostVariants, parseHostVariant, variantCapabilities, type HostVariant } from "./variants";
const hosts: SimHost[] = [];
afterEach(() => { hosts.splice(0).forEach((h) => h.dispose()); });
it.each(Object.keys(hostVariants) as HostVariant[])("reports %s consistently in host and readiness", (variant) => {
  const host = new SimHost({ variant }); hosts.push(host);
  expect(host.host.os_label).toBe(hostVariants[variant].label);
  expect(host.host.capabilities).toMatchObject({ os: hostVariants[variant].os, desktop_backend: hostVariants[variant].backend, input_scope: "focused_target", computer_use: { state: "available" } });
  expect(host.readiness.checks.some((c) => c.label === `Screen capture (${hostVariants[variant].backend})`)).toBe(true);
});
it.each(Object.keys(hostVariants) as HostVariant[])("uses the native protection failure reason for %s", (variant) => {
  const host = new SimHost({ variant, indicator: false }); hosts.push(host);
  expect(host.host.capabilities.input).toEqual({ state: "temporarily_unavailable", reason: hostVariants[variant].stopReason });
  expect(host.host.capabilities.computer_use).toEqual(host.host.capabilities.input);
  expect(host.host.capabilities.capture.state).toBe("available");
});
it("handles unknown query values without inherited object keys", () => {
  for (const value of [null, "", "unknown", "toString", "__proto__"]) expect(parseHostVariant(value)).toBe("hyprland");
  expect(parseHostVariant("windows")).toBe("windows");
});
it("offers unlocking with a password only where the computer checks one", () => {
  expect(variantCapabilities(capabilities(), "windows").unlock_session?.state).toBe("unsupported");
  for (const linux of ["hyprland", "gnome", "kde"] as const) {
    expect(variantCapabilities(capabilities(), linux).unlock_session?.state).toBe("available");
  }
});
