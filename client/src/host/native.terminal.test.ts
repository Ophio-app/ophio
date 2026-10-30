// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createNativePlatform, LINK_CHECK_MS } from "./native";

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


it("releases the terminal native slot before opening its replacement", async () => {
  const platform = createNativePlatform();
  const first = platform.terminal("terminal-slot-test");
  await tick();
  const events = linkEvents!;
  events.onmessage({ type: "authenticated", challenge: {}, auth: {} });
  let finishClose: () => void = () => {};
  let finishSend: (sent: boolean) => void = () => {};
  native.invoke.mockImplementation((command: string) => {
    if (command === "link_close") return new Promise<void>(resolve => { finishClose = resolve; });
    if (command === "link_send") return new Promise<boolean>(resolve => { finishSend = resolve; });
    if (command === "link_open") return Promise.resolve(2);
    return Promise.resolve();
  });
  const listener = vi.fn(); first.listen(listener);
  first.send({ type: "resize", cols: 100, rows: 30 }); await tick();
  first.close();
  const next = platform.terminal("terminal-slot-test");
  await tick();
  expect(native.invoke.mock.calls.filter(([name]) => name === "link_open")).toHaveLength(1);
  finishSend(false); await tick();
  events.onmessage({ type: "state", state: { state: "offline", reason: "Connection interrupted" } });
  expect(listener).not.toHaveBeenCalled();
  finishClose(); await tick();
  expect(native.invoke.mock.calls.filter(([name]) => name === "link_open")).toHaveLength(2);
  next.close(); await tick(); finishClose(); await tick();
});

it("opens with its page, and Retry connection opens again after a refusal", async () => {
  // A new page: none asked for its number yet.
  vi.resetModules();
  const platform = (await import("./native")).createNativePlatform();
  let page = 4;
  const opens: unknown[] = [];
  const connects: unknown[] = [];
  const closes: unknown[] = [];
  const closed = { message: "The connection closed before the computer confirmed the operation." };
  native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "link_page") return page;
    if (command === "link_open") {
      opens.push(args?.page);
      // A page counted before its own load began is refused, as after a reload.
      if (opens.length === 1) throw closed;
      return 7;
    }
    if (command === "link_connect") {
      connects.push(args?.linkId);
      if (connects.length === 1) throw closed;
      return undefined;
    }
    if (command === "link_close") closes.push(args?.linkId);
    return undefined;
  });
  const control = platform.control("retry-after-refusal");
  const states: string[] = [];
  control.listen((e) => { if (e.type === "state") states.push(e.state.state); });
  control.connect(); await tick();
  expect(opens).toEqual([4]);
  expect(connects).toEqual([]);
  expect(states.at(-1)).toBe("offline");
  // The native side has counted this page since; the next try asks again.
  page = 5;
  control.connect(); await tick(); await tick();
  expect(opens).toEqual([4, 5]);
  expect(connects).toEqual([7]);
  // The native link has ended, so the try after opens one in its place.
  control.connect(); await tick(); await tick();
  expect(closes).toEqual([7]);
  expect(opens).toEqual([4, 5, 5]);
  expect(connects).toEqual([7, 7]);
  control.disconnect();
});

it("keeps the code of a native refusal, so pairing can say how it ended", async () => {
  const platform = createNativePlatform();
  native.invoke.mockImplementation(async (command: string) => {
    if (command === "pair") throw { code: "permission_denied", message: "The computer's owner refused this device", detail: { reason: null } };
    if (command === "pair_cancel") throw "not an object";
    return undefined;
  });
  const offer = { host_id: "host", host_name: "Desk", cert_sha256: "00".repeat(32), addresses: ["127.0.0.1:1"],
    pairing_secret: "secret", expires_at: Date.now() + 60_000, protocol: { major: 1, minor: 0 } };
  const refused = await platform.pair(offer, "Phone", false, () => {}).catch((e: unknown) => e);
  expect(refused).toBeInstanceOf(Error);
  expect(refused).toMatchObject({ code: "permission_denied", message: "The computer's owner refused this device" });
  const untyped = await platform.cancelPairing().catch((e: unknown) => e);
  expect(untyped).toBeInstanceOf(Error);
  expect((untyped as Error).message).toBe("This device could not complete this operation.");
  expect("code" in (untyped as Error)).toBe(false);
});

/** A native side whose links each report how many messages they sent. */
function countingLinks() {
  const opened: { onmessage: (value: unknown) => void }[] = [];
  const sent: Record<number, number> = {};
  const closes: unknown[] = [];
  const connects: unknown[] = [];
  native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "link_open") {
      opened.push(args?.events as (typeof opened)[number]);
      return opened.length;
    }
    if (command === "link_sent") return sent[args?.linkId as number] ?? 0;
    if (command === "link_connect") connects.push(args?.linkId);
    if (command === "link_close") closes.push(args?.linkId);
    return undefined;
  });
  return { opened, sent, closes, connects };
}

it("opens the control link again once a message it was sent never arrives", async () => {
  vi.useFakeTimers();
  try {
    const links = countingLinks();
    const control = createNativePlatform().control("stalled-channel");
    const events: string[] = [];
    control.listen((e) => events.push(e.type === "state" ? e.state.state : e.type));
    control.connect(); await tick();
    // The native side reached the computer, but the webview lost the message
    // that it was checking the computer's identity, and holds back the rest.
    links.opened[0]!.onmessage({ type: "state", state: { state: "connecting", attempt: 1 } });
    links.sent[1] = 3;
    await vi.advanceTimersByTimeAsync(LINK_CHECK_MS); await tick();
    expect(links.closes).toEqual([]);
    await vi.advanceTimersByTimeAsync(LINK_CHECK_MS); await tick();
    expect(links.closes).toEqual([1]);
    expect(links.opened).toHaveLength(2);
    expect(links.connects).toEqual([1, 2]);
    expect(events).toEqual(["connecting", "connecting"]);
    // The first channel's late messages are dropped; the new one's arrive.
    links.opened[0]!.onmessage({ type: "state", state: { state: "authenticating" } });
    links.opened[1]!.onmessage({ type: "authenticated", challenge: {}, auth: {} });
    links.sent[2] = 1;
    expect(events).toEqual(["connecting", "connecting", "authenticated"]);
    await vi.advanceTimersByTimeAsync(3 * LINK_CHECK_MS); await tick();
    expect(links.opened).toHaveLength(2);
    control.disconnect();
  } finally {
    vi.useRealTimers();
  }
});

it("leaves a link alone while its messages are still arriving or the page is hidden", async () => {
  vi.useFakeTimers();
  try {
    const links = countingLinks();
    const control = createNativePlatform().control("slow-channel");
    control.connect(); await tick();
    links.opened[0]!.onmessage({ type: "state", state: { state: "connecting", attempt: 1 } });
    links.sent[1] = 5;
    await vi.advanceTimersByTimeAsync(LINK_CHECK_MS); await tick();
    // Slow is not lost: one more arrived since the last check.
    links.opened[0]!.onmessage({ type: "state", state: { state: "authenticating" } });
    await vi.advanceTimersByTimeAsync(LINK_CHECK_MS); await tick();
    expect(links.closes).toEqual([]);
    // A hidden page may be held back; it isn't judged until it is shown.
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
    await vi.advanceTimersByTimeAsync(3 * LINK_CHECK_MS); await tick();
    expect(links.closes).toEqual([]);
    Reflect.deleteProperty(document, "visibilityState");
    await vi.advanceTimersByTimeAsync(LINK_CHECK_MS); await tick();
    expect(links.closes).toEqual([]);
    await vi.advanceTimersByTimeAsync(LINK_CHECK_MS); await tick();
    expect(links.closes).toEqual([1]);
    control.disconnect();
  } finally {
    Reflect.deleteProperty(document, "visibilityState");
    vi.useRealTimers();
  }
});

it("opens web pages, and the emails and calls a checklist step starts, in the phone's own apps", async () => {
  const platform = createNativePlatform();
  for (const url of ["https://namecheap.com/", "mailto:security@ophio.app?subject=Test", "tel:+15551234567"]) {
    await platform.openExternal(url);
    expect(native.invoke).toHaveBeenLastCalledWith("plugin:opener|open_url", { url, with: null });
  }
  native.invoke.mockClear();
  await expect(platform.openExternal("file:///etc/passwd")).rejects.toThrow("Only web, email and phone addresses can be opened.");
  await expect(platform.openExternal("https://me:secret@example.com/")).rejects.toThrow();
  expect(native.invoke).not.toHaveBeenCalledWith("plugin:opener|open_url", expect.anything());
});
