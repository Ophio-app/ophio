// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it } from "vitest";
import { SimHost } from "../../dev/sim/host";
import { ids } from "../../dev/sim/scenario";
import type { WindowControl } from "../../protocol/types";
import { previewCloseText } from "./previewDrafts";
const hosts: SimHost[] = [];
afterEach(() => {
  for (const host of hosts) host.dispose();
  hosts.length = 0;
});
describe("Window control contract", () => {
  it("keeps GNOME app launch available without inventing a window list", () => {
    const host = new SimHost();
    hosts.push(host);
    host.windows.backend = "gnome";
    const list = host.windows.catalog("firefox");
    expect(list.capabilities.windows.state).toBe("unsupported");
    expect(list.windows).toEqual([]);
    expect(list.apps.map((a) => a.name)).toEqual(["Firefox"]);
  });
  it("records dispatched close requests and refuses a stale generation", async () => {
    const host = new SimHost();
    hosts.push(host);
    const view = host.control.find((v) => v.target_id === ids.tgHdmi);
    const target = host.targets.find((t) => t.target_id === ids.tgHdmi);
    if (!view || !target) throw new Error("Missing fixture target");
    view.input_state = "human";
    view.lease = {
      lease_id: "lease",
      target_id: target.target_id,
      boot_epoch: host.bootEpoch,
      generation: 4,
      owner: { kind: "human", device_id: ids.device },
      state: "active",
      granted_at: Date.now(),
      renewal_window_ms: 2500,
    };
    const control: WindowControl = {
      target_id: target.target_id,
      lease_id: "lease",
      boot_epoch: host.bootEpoch,
      generation: 4,
      geometry_revision: target.geometry.revision,
    };
    const receipt = host.windows.act(control, {
      action: "close",
      window_id: "editor",
    });
    expect(receipt.outcome).toBe("close_requested");
    expect(
      host.windows.catalog("").windows.some((w) => w.window_id === "editor"),
    ).toBe(true);
    expect(() =>
      host.windows.act(
        { ...control, generation: 3 },
        { action: "focus", window_id: "editor" },
      ),
    ).toThrow();
    expect(host.windows.actions).toHaveLength(1);
  });
  it("distinguishes lost connection, idle, revocation and removal", () => {
    expect(previewCloseText("idle_timeout")).toContain("idle");
    expect(previewCloseText("device_revoked")).toContain("revoked");
    expect(previewCloseText("removed")).toContain("removed");
    expect(previewCloseText("lost_connection")).toContain("lost");
  });
});
