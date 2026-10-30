// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createNativePlatform } from "../../src/host/native";

const native = vi.hoisted(() => ({
  invoke: vi.fn(),
  channels: [] as { onmessage: (value: unknown) => void }[],
}));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {
    onmessage = (_value: unknown) => {};
    constructor() {
      native.channels.push(this);
    }
  },
  invoke: native.invoke,
}));
const tick = async () => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};
let linkEvents: { onmessage: (value: unknown) => void } | undefined;
let deviceEvents: { onmessage: (value: unknown) => void } | undefined;
beforeEach(() => {
  linkEvents = undefined;
  deviceEvents = undefined;
  native.invoke.mockReset();
  native.invoke.mockImplementation(
    async (command: string, args?: Record<string, unknown>) => {
      if (command === "link_open") {
        linkEvents = args?.events as typeof linkEvents;
        return 1;
      }
      if (command === "device_events")
        deviceEvents = args?.events as typeof deviceEvents;
      return undefined;
    },
  );
});
afterEach(() => {
  vi.restoreAllMocks();
});

describe("native platform IPC", () => {
  it("paced chunks reflect the native boolean and remain pending until the socket acknowledges", async () => {
    const platform = createNativePlatform();
    const transfer = platform.transfer("one");
    await tick();
    linkEvents?.onmessage({ type: "authenticated", challenge: {}, auth: {} });
    let written: ((ok: boolean) => void) | undefined;
    native.invoke.mockImplementation((command: string) =>
      command === "link_send_binary"
        ? new Promise<boolean>((resolve) => {
            written = resolve;
          })
        : Promise.resolve(),
    );
    let settled = false;
    const pending = transfer.chunk(new Uint8Array([1, 2])).then((ok) => {
      settled = true;
      return ok;
    });
    await tick();
    expect(settled).toBe(false);
    written?.(false);
    expect(await pending).toBe(false);
    transfer.close();
    expect(await transfer.chunk(new Uint8Array([1]))).toBe(false);
  });
  it("reports native pairing explanations as Error objects", async () => {
    const platform = createNativePlatform();
    native.invoke.mockRejectedValue({
      code: "unauthenticated",
      message: "Certificate mismatch",
    });
    await expect(platform.profiles()).rejects.toThrow("Certificate mismatch");
  });
  it("buffers shares before the inbox subscribes, delivers once, then acknowledges", async () => {
    const platform = createNativePlatform();
    await tick();
    const item = {
      id: "share-id",
      name: "Shared text",
      mime: "text/plain",
      text: "synthetic share",
      size: 0,
    };
    deviceEvents?.onmessage({ type: "shared", items: [item, item] });
    const listener = vi.fn();
    platform.onShared?.(listener);
    await tick();
    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener.mock.calls[0]?.[0]).toEqual([
      {
        id: "share-id",
        name: "Shared text",
        mime: "text/plain",
        text: "synthetic share",
      },
    ]);
    expect(native.invoke).toHaveBeenCalledWith("shared_ack", {
      id: "share-id",
    });
    deviceEvents?.onmessage({ type: "shared", items: [item] });
    await tick();
    expect(listener).toHaveBeenCalledTimes(1);
  });
  it("does not acknowledge a share before its durable consumer commits", async () => {
    const platform = createNativePlatform();
    await tick();
    let commit!: () => void;
    platform.onShared?.(
      () =>
        new Promise<void>((resolve) => {
          commit = resolve;
        }),
    );
    deviceEvents?.onmessage({
      type: "shared",
      items: [
        {
          id: "durable",
          name: "Idea",
          mime: "text/plain",
          text: "fixture",
          size: 7,
        },
      ],
    });
    await tick();
    expect(
      native.invoke.mock.calls.some(([name]) => name === "shared_ack"),
    ).toBe(false);
    commit();
    await tick();
    expect(native.invoke).toHaveBeenCalledWith("shared_ack", { id: "durable" });
  });
  it("requests notification permission separately from task notification delivery", async () => {
    const platform = createNativePlatform();
    await platform.requestNotifications?.();
    expect(native.invoke).toHaveBeenCalledWith("request_notifications");
    expect(native.invoke.mock.calls.some(([name]) => name === "notify")).toBe(
      false,
    );
  });
  it("cleans up an incomplete MediaStore download when a chunk fails", async () => {
    const platform = createNativePlatform();
    native.invoke.mockImplementation(async (command: string) => {
      if (command === "save_begin") return { id: "download" };
      if (command === "save_chunk") throw new Error("Storage full");
    });
    // jsdom's Blob has no arrayBuffer; use a structural native Blob fixture.
    const blob = {
      size: 2,
      type: "text/plain",
      slice: () => ({ arrayBuffer: async () => new Uint8Array([1, 2]).buffer }),
    } as Blob;
    await expect(platform.saveToPhone?.(blob, "check.txt")).rejects.toThrow(
      "Storage full",
    );
    expect(native.invoke).toHaveBeenCalledWith("save_cancel", {
      id: "download",
    });
    expect(
      native.invoke.mock.calls.some(([name]) => name === "save_finish"),
    ).toBe(false);
  });
  it("buffers notification destinations without taking action", async () => {
    const platform = createNativePlatform();
    await tick();
    const notification = { id: "attention", route: { screen: "work" } };
    deviceEvents?.onmessage({ type: "notificationOpen", notification });
    const open = vi.fn();
    platform.onNotificationOpen?.(open);
    await tick();
    expect(open).toHaveBeenCalledExactlyOnceWith(notification);
    expect(
      native.invoke.mock.calls.some(([name]) => name === "link_send"),
    ).toBe(false);
  });
  it("retains device failures until the app attaches its error listener", async () => {
    native.invoke.mockRejectedValueOnce({
      code: "native_failure",
      message: "Device events unavailable",
    });
    const platform = createNativePlatform();
    await tick();
    const errors = vi.fn();
    platform.onPlatformError?.(errors);
    await tick();
    expect(errors).toHaveBeenCalledExactlyOnceWith({
      operation: "Device events",
      message: "Device events unavailable",
    });
  });
});
