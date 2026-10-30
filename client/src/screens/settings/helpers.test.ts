// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it, vi } from "vitest";
import { classifyPreviewUrl, withGrant } from "./helpers";
import { SimHost } from "../../dev/sim/host";
import { uuidv7 } from "../../protocol/ids";
import type { Command } from "../../protocol/types";
describe("preview URL disclosure", () => {
  it.each([
    "http://localhost:3000",
    "http://127.0.0.1",
    "https://10.2.3.4",
    "http://192.168.1.4",
    "http://172.16.0.1",
    "http://100.64.0.1",
    "https://work.tail123.ts.net",
    "http://[::1]",
    "http://[fd7a:115c::1]",
  ])("recognizes private hosts: %s", (url) => {
    expect(classifyPreviewUrl(url)).toMatchObject({
      valid: true,
      privateHost: true,
    });
  });
  it.each([
    "https://example.com",
    "http://172.32.0.1",
    "http://100.128.0.1",
    "https://evil.ts.net.example.com",
    "https://localhost.example.com",
  ])("warns for a public host: %s", (url) => {
    expect(classifyPreviewUrl(url)).toMatchObject({
      valid: true,
      privateHost: false,
    });
  });
  it.each([
    "javascript:alert(1)",
    "file:///tmp/a",
    "data:text/html,hi",
    "https://user:secret@example.com",
    "bad",
  ])("refuses unsuitable URLs", (url) => {
    expect(classifyPreviewUrl(url).valid).toBe(false);
  });
});
it("simulates confirmed access changes and rejects a bad chat code", async () => {
  const host = new SimHost();
  const send = (body: Command) =>
    host.command({
      command_id: uuidv7(Date.now()),
      issued_at: Date.now(),
      expected_revision: null,
      expires_at: null,
      body,
    });
  try {
    const other = host.devices.find((d) => !d.is_current && !d.revoked_at);
    expect(other).toBeDefined();
    if (!other) return;
    await send({
      command: "rename_device",
      device_id: other.device_id,
      name: "Tablet",
    });
    expect(
      host.devices.find((d) => d.device_id === other.device_id)?.name,
    ).toBe("Tablet");
    expect(
      (await send({ command: "approve_channel_link", link_code: "bad" }))
        .status,
    ).toBe("rejected");
    expect(
      (await send({ command: "approve_channel_link", link_code: "123-456" }))
        .status,
    ).toBe("accepted");
    await send({
      command: "set_device_grants",
      expected: other.grants,
      device_id: other.device_id,
      grants: withGrant(other.grants, "control_input", true),
    });
    await vi.waitFor(() =>
      expect(
        host.devices.find((d) => d.device_id === other.device_id)?.grants,
      ).toMatchObject({ control_input: true, view: true }),
    );
    const revoke = await send({
      command: "revoke_device",
      device_id: other.device_id,
    });
    // Like the companion, revocation settles after admission.
    expect(revoke.result).toBeNull();
    await vi.waitFor(() =>
      expect(
        host.devices.find((d) => d.device_id === other.device_id)?.revoked_at,
      ).not.toBeNull(),
    );
    expect(host.journal.some((e) => e.event.type === "devices_changed")).toBe(
      true,
    );
    // A revoked device is paired again, not given access back.
    const regrant = await send({
      command: "set_device_grants",
      expected: other.grants,
      device_id: other.device_id,
      grants: other.grants,
    });
    expect(regrant.status).toBe("rejected");
    const preview = host.previews[0];
    if (!preview) return;
    await send({ command: "check_preview", preview_id: preview.preview_id });
    expect(host.previews[0]?.checked_at).toBeGreaterThan(
      preview.checked_at ?? 0,
    );
    await send({ command: "remove_preview", preview_id: preview.preview_id });
    expect(host.previews.some((p) => p.preview_id === preview.preview_id)).toBe(
      false,
    );
  } finally {
    host.dispose();
  }
});
it("keeps input control with a view and Library editing with reading", () => {
  const none = {
    view: false,
    control_input: false,
    terminal: false,
    tasks: false,
    approvals: false,
    computer_use: false,
    library_read: false,
    library_write: false,
    transfers: false,
    admin: false,
    accounts: false,
  };
  expect(withGrant(none, "control_input", true)).toMatchObject({ control_input: true, view: true });
  expect(withGrant(none, "library_write", true)).toMatchObject({ library_write: true, library_read: true });
  const all = Object.fromEntries(Object.keys(none).map((k) => [k, true])) as typeof none;
  expect(withGrant(all, "view", false)).toMatchObject({ view: false, control_input: false, tasks: true });
  expect(withGrant(all, "library_read", false)).toMatchObject({ library_read: false, library_write: false });
});
