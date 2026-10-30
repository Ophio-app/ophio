// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { HostProfile, Platform } from "../../host/link";
import type { PairingOffer } from "../../protocol/types";
import { Welcome } from "./Welcome";
// Keyboard and cramped-height layout is checked in the browser; its focus
// tracking would otherwise measure after this file's document is gone.
vi.mock("../../shell/keyboard", () => ({ useKeyboardOpen: () => false, useCrampedLayout: () => false }));
afterEach(cleanup);

it.each([false, true])("two system Backs traverse confirmation and connect (adding computer: %s)", async (adding) => {
  localStorage.clear();
  Element.prototype.scrollTo ??= () => {};
  const offer: PairingOffer = { host_id: "host", host_name: "Desk", cert_sha256: "00".repeat(32),
    addresses: ["127.0.0.1"], pairing_secret: "fixture", expires_at: Date.now() + 60_000,
    protocol: { major: 1, minor: 0 } };
  const platform = { profiles: async () => [], parsePairingLink: () => ({ offer }) } as unknown as Platform;
  const cancel = vi.fn();
  // Cancelling closes pairing, as App does.
  function Pairing() {
    const [pairing, setPairing] = useState(true);
    return pairing && <Welcome platform={platform} onPaired={() => {}}
      onCancel={adding ? () => { cancel(); setPairing(false); } : undefined} />;
  }
  render(<Pairing />);
  if (!adding) fireEvent.click(screen.getByRole("button", { name: "Pair a computer" }));
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  expect(screen.queryByRole("heading", { name: "Pair with your computer" })).toBeNull();
  act(() => window.dispatchEvent(new PopStateEvent("popstate")));
  expect(screen.getByRole("heading", { name: "Pair with your computer" })).toBeTruthy();
  act(() => window.dispatchEvent(new PopStateEvent("popstate")));
  if (adding) {
    expect(cancel).toHaveBeenCalledOnce();
    expect(screen.queryByRole("heading", { name: "Pair with your computer" })).toBeNull();
  } else expect(screen.getByRole("heading", { name: /Your computer/ })).toBeTruthy();
});

it("system back steps back through pairing instead of leaving the app", () => {
  localStorage.clear();
  Element.prototype.scrollTo ??= () => {};
  const platform = { parsePairingLink: () => ({ problem: "not-pairing" }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Pair a computer" }));
  expect(screen.getByRole("heading", { name: "Pair with your computer" })).toBeTruthy();
  expect(screen.getByText("Install Øphio on your computer")).toBeTruthy();
  expect(screen.getByText("INSTALL.md")).toBeTruthy();
  expect(screen.getByText("ophio pair")).toBeTruthy();
  act(() => {
    window.dispatchEvent(new PopStateEvent("popstate"));
  });
  expect(screen.getByRole("heading", { name: /Your computer/ })).toBeTruthy();
});

it("warns by host identity even when its displayed name changed, and pairs again only in place of the earlier pairing", async () => {
  localStorage.clear();
  const profile: HostProfile = { profileId: "p", hostId: "host", name: "Desk", pairedAt: 1700000000000,
    certSha256: "00".repeat(32), deviceId: "phone", addresses: ["127.0.0.1"] };
  const offer: PairingOffer = { host_id: "host", host_name: "Renamed desk", cert_sha256: profile.certSha256,
    addresses: profile.addresses, pairing_secret: "fixture", expires_at: Date.now() + 60_000, protocol: { major: 1, minor: 0 } };
  const pair = vi.fn(async () => { throw new Error("stop here"); });
  const platform = { profiles: async () => [profile], pair, parsePairingLink: () => ({ offer }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  expect(screen.getByText(`Already paired with Desk since ${new Date(profile.pairedAt).toLocaleDateString()}`)).toBeTruthy();
  expect(screen.getByText(/keeps one pairing for each computer, so pairing again replaces that one here/)).toBeTruthy();
  expect(screen.getByText(/When you confirm on the computer, it removes the earlier device and gives this pairing that device’s permissions/)).toBeTruthy();
  const again = screen.getByRole<HTMLButtonElement>("button", { name: "Pair again" });
  expect(again.disabled).toBe(true);
  await act(async () => { fireEvent.click(again); });
  expect(pair).not.toHaveBeenCalled();
  fireEvent.click(screen.getByLabelText("Replace the earlier pairing on this phone"));
  expect(again.disabled).toBe(false);
  await act(async () => { fireEvent.click(again); });
  expect(pair).toHaveBeenCalledOnce();
  expect((pair.mock.calls[0] as unknown[])[2]).toBe(true);
});

it("pairs a new computer without replacing anything", async () => {
  const { pairAs, pair } = await confirmWith(info("Galaxy S24"));
  expect(screen.queryByLabelText(/Replace the earlier pairing/)).toBeNull();
  await pairAs();
  expect((pair.mock.calls[0] as unknown[])[2]).toBe(false);
});

it("says the device is full before it asks the computer to enroll it", async () => {
  localStorage.clear();
  const profiles: HostProfile[] = Array.from({ length: 32 }, (_, n) => ({ profileId: `p${n}`, hostId: `host${n}`, name: `Computer ${n}`,
    pairedAt: 1, certSha256: "00".repeat(32), deviceId: "phone", addresses: ["127.0.0.1"] }));
  const pair = vi.fn();
  const platform = { profiles: async () => profiles, pair, parsePairingLink: () => ({ offer: liveOffer() }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  expect(screen.getByText("This phone already remembers 32 computers")).toBeTruthy();
  expect(screen.getByText("Forget one of them in Connections, then scan a new code to pair with Desk.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Pair" })).toBeNull();
  expect(screen.queryByLabelText("Name this phone")).toBeNull();
  expect(pair).not.toHaveBeenCalled();
});

it("pairs again with one of 32 computers in place of its earlier pairing", async () => {
  localStorage.clear();
  const profiles: HostProfile[] = Array.from({ length: 32 }, (_, n) => ({ profileId: `p${n}`, hostId: n === 7 ? "host" : `host${n}`,
    name: `Computer ${n}`, pairedAt: 1, certSha256: "00".repeat(32), deviceId: "phone", addresses: ["127.0.0.1"] }));
  const pair = vi.fn(async () => { throw new Error("stop here"); });
  const platform = { profiles: async () => profiles, pair, parsePairingLink: () => ({ offer: liveOffer() }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  expect(screen.queryByText(/already remembers 32 computers/)).toBeNull();
  fireEvent.click(screen.getByLabelText("Replace the earlier pairing on this phone"));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Pair again" })); });
  expect((pair.mock.calls[0] as unknown[])[2]).toBe(true);
});

it.each(["", "https://downloads.example.test/ophio"])("offers configured acquisition and plain-text sharing (%s)", async url => {
  localStorage.clear();
  vi.stubEnv("VITE_OPHIO_DOWNLOAD_URL", url);
  vi.stubEnv("VITE_OPHIO_CHECKSUM_URL", "https://downloads.example.test/checksums");
  const openExternal = vi.fn(async () => {});
  const shareText = vi.fn(async (_text: string) => "shared" as const);
  const platform = { openExternal, shareText } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} />);
  fireEvent.click(screen.getByText("Get Øphio for your computer"));
  expect(screen.getByRole("button", { name: "Send these steps to your computer" })).toBeTruthy();
  if (url) {
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open download page" })));
    expect(openExternal).toHaveBeenCalledWith(url);
    expect(screen.getByRole("button", { name: "Check the archive checksum" })).toBeTruthy();
  } else {
    expect(screen.getByText(/This build has no download link for Øphio/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open download page" })).toBeNull();
  }
  fireEvent.click(screen.getByRole("button", { name: "Pair a computer" }));
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Send these steps to your computer" })));
  const text = shareText.mock.calls[0]![0];
  expect(text).toContain("INSTALL.md");
  expect(text).toContain("ophio pair");
  expect(text).not.toMatch(/<\/?(?:p|a|ol|li)>/);
  expect(text).toContain(url || "This build has no download link for Øphio");
  vi.unstubAllEnvs();
});

it("takes a pasted link as it is, without keyboard suggestions or corrections", () => {
  localStorage.clear();
  render(<Welcome platform={{ parsePairingLink: () => ({ problem: "not-pairing" }) } as unknown as Platform} onPaired={() => {}} onCancel={() => {}} />);
  const field = screen.getByLabelText("Pairing link");
  expect(field.getAttribute("autocomplete")).toBe("off");
  expect(field.getAttribute("autocorrect")).toBe("off");
  expect(field.getAttribute("autocapitalize")).toBe("off");
  expect(field.getAttribute("spellcheck")).toBe("false");
  // A url input mode would override the no-suggestions flag on Android.
  expect(field.hasAttribute("inputmode")).toBe(false);
});

const expiredOffer: PairingOffer = { host_id: "host", host_name: "Desk", cert_sha256: "00".repeat(32), addresses: ["127.0.0.1"],
  pairing_secret: "fixture", expires_at: Date.now() - 1_000, protocol: { major: 1, minor: 0 } };

it.each([
  [{ problem: "not-pairing" }, "That isn’t a pairing link", "Copy it again from ophio pair on the computer, or scan the code."],
  [{ problem: "incomplete" }, "That pairing link is incomplete", "Copy it again from ophio pair on the computer, or scan the code."],
  [{ offer: expiredOffer }, "That pairing link has expired", "Run ophio pair on the computer again, then copy the new link or scan the new code."],
])("says why a pasted link was refused and what to do (%o)", async (parsed, title, next) => {
  localStorage.clear();
  const platform = { parsePairingLink: () => parsed, scanPairingCode: async () => null } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Paste the link instead" }));
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  expect(screen.getByRole("alert").textContent).toBe(`${title}${next}Report`);
  expect(screen.getByRole("heading", { name: "Pair with your computer" })).toBeTruthy();
});

it.each([
  [{ problem: "not-pairing" }, "That isn’t a pairing code", "Scan the code that ophio pair shows on the computer."],
  [{ offer: expiredOffer }, "That pairing code has expired", "Run ophio pair on the computer again, then scan the new code."],
])("says why a scanned code was refused and what to do (%o)", async (parsed, title, next) => {
  localStorage.clear();
  const platform = { parsePairingLink: () => parsed, scanPairingCode: async () => "fixture" } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Scan the code" })); });
  expect(screen.getByRole("alert").textContent).toBe(`${title}${next}Report`);
});

it("brings the reason into view each time a link is refused", async () => {
  localStorage.clear();
  const scrolled = vi.fn();
  Element.prototype.scrollIntoView = scrolled;
  try {
    const platform = { parsePairingLink: () => ({ problem: "incomplete" }) } as unknown as Platform;
    render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
    fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
    expect(scrolled).toHaveBeenCalledTimes(2);
    expect(scrolled.mock.contexts[1]).toBe(screen.getByRole("alert").parentElement);
    expect(scrolled).toHaveBeenLastCalledWith({ block: "start" });
  } finally {
    delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  }
});

it("puts the focus in the link field and brings it into view when the person chooses to paste", () => {
  localStorage.clear();
  const scrolled = vi.fn();
  Element.prototype.scrollIntoView = scrolled;
  try {
    const platform = { parsePairingLink: () => ({ problem: "incomplete" }), scanPairingCode: async () => null } as unknown as Platform;
    render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
    expect(scrolled).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Paste the link instead" }));
    const field = screen.getByLabelText("Pairing link");
    expect(document.activeElement).toBe(field);
    // The whole field, hint included, not just the input.
    expect(scrolled.mock.contexts[0]).toBe(field.parentElement);
    expect(field.parentElement?.textContent).toContain("Above the QR code on the computer");
  } finally {
    delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  }
});

it("leaves the focus alone when the field is shown because this device can't scan", () => {
  localStorage.clear();
  render(<Welcome platform={{ parsePairingLink: () => ({ problem: "incomplete" }) } as unknown as Platform} onPaired={() => {}} onCancel={() => {}} />);
  expect(document.activeElement).not.toBe(screen.getByLabelText("Pairing link"));
});

it("offers only copying again when this device can't scan", async () => {
  localStorage.clear();
  const platform = { parsePairingLink: () => ({ problem: "incomplete" }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  expect(screen.getByRole("alert").textContent).toBe("That pairing link is incompleteCopy it again from ophio pair on the computer.Report");
});

const liveOffer = (): PairingOffer => ({ host_id: "host", host_name: "Desk", cert_sha256: "00".repeat(32), addresses: ["127.0.0.1"],
  pairing_secret: "fixture", expires_at: Date.now() + 60_000, protocol: { major: 1, minor: 0 } });

async function confirmWith(appInfo: Platform["appInfo"]) {
  localStorage.clear();
  const pair = vi.fn(async () => { throw new Error("stop here"); });
  const platform = { profiles: async () => [], pair, appInfo, parsePairingLink: () => ({ offer: liveOffer() }) } as unknown as Platform;
  render(<Welcome platform={platform} onPaired={() => {}} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  const field = screen.getByLabelText<HTMLInputElement>("Name this phone");
  const pairAs = async () => {
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Pair" })); });
    return (pair.mock.calls.at(-1) as unknown[] | undefined)?.[1];
  };
  return { field, pairAs, pair };
}

const info = (deviceName?: string) => async () => ({ version: "1", build: "1", keyProtection: "hardware", deviceName });

it("names the paired device after the name it has in its own settings", async () => {
  const { field, pairAs } = await confirmWith(info("  Galaxy S24  "));
  expect(field.value).toBe("Galaxy S24");
  expect(await pairAs()).toBe("Galaxy S24");
});

it.each([
  ["reports no name", info()],
  ["reports a blank name", info("   ")],
  ["can't say", async () => { throw new Error("unavailable"); }],
  ["has no app details", undefined],
])("falls back to the device's kind when the platform %s", async (_case, appInfo) => {
  const { field, pairAs } = await confirmWith(appInfo as Platform["appInfo"]);
  expect(field.value).toBe("Phone");
  expect(await pairAs()).toBe("Phone");
});

it("keeps the name the person typed, even when the device's name arrives later", async () => {
  let report: (name: string) => void = () => {};
  const late = () => new Promise<{ version: string; build: string; keyProtection: string; deviceName: string }>(resolve => {
    report = deviceName => resolve({ version: "1", build: "1", keyProtection: "hardware", deviceName });
  });
  const { field, pairAs } = await confirmWith(late);
  fireEvent.change(field, { target: { value: "Kitchen tablet" } });
  await act(async () => report("Galaxy S24"));
  expect(field.value).toBe("Kitchen tablet");
  expect(await pairAs()).toBe("Kitchen tablet");
});

it("pairs under the device's name when the person clears the field", async () => {
  const { field, pairAs } = await confirmWith(info("Galaxy S24"));
  fireEvent.change(field, { target: { value: "  " } });
  expect(await pairAs()).toBe("Galaxy S24");
});

/** A platform whose computer holds the request until the test answers for its owner. */
function heldPairing({ reached = true } = {}) {
  let answer: { resolve: (r: unknown) => void; reject: (e: unknown) => void } | null = null;
  const cancelPairing = vi.fn(async () => {});
  const pair = vi.fn((_offer: PairingOffer, _name: string, _replace: boolean, waiting: (code: string) => void) => {
    // The code arrives once the pinned computer is reached.
    if (reached) waiting("042917");
    return new Promise((resolve, reject) => { answer = { resolve, reject }; });
  });
  const platform = { profiles: async () => [], pair, cancelPairing, parsePairingLink: () => ({ offer: liveOffer() }) } as unknown as Platform;
  return { platform, pair, cancelPairing, answer: () => answer! };
}

async function askToPair(platform: Platform, onPaired = () => {}) {
  localStorage.clear();
  Element.prototype.scrollTo ??= () => {};
  render(<Welcome platform={platform} onPaired={onPaired} onCancel={() => {}} />);
  fireEvent.change(screen.getByLabelText("Pairing link"), { target: { value: "fixture" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Continue" })); });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Pair" })); });
}

it("shows the code to compare while the computer's owner decides, then what the computer allowed", async () => {
  const held = heldPairing();
  await askToPair(held.platform);
  expect(screen.getByRole("heading", { name: "Check the code on Desk" })).toBeTruthy();
  expect(screen.getByLabelText("Code 0 4 2 9 1 7").textContent).toBe("042 917");
  expect(screen.getByRole("status").textContent).toBe("Waiting for your answer on Desk.");
  const profile: HostProfile = { profileId: "p", hostId: "host", name: "Desk", pairedAt: 1, certSha256: "00".repeat(32), deviceId: "phone", addresses: ["127.0.0.1"] };
  const grants = { view: true, control_input: false, terminal: false, tasks: true, approvals: true, computer_use: false, library_read: true, library_write: false, transfers: true, admin: false };
  await act(async () => held.answer().resolve({ profile, response: { device_id: "phone", host_id: "host", host_name: "Desk", grants } }));
  expect(screen.getByRole("heading", { name: "Paired with Desk" })).toBeTruthy();
  expect(held.cancelPairing).not.toHaveBeenCalled();
});

it.each([
  [{ code: "permission_denied", message: "The computer's owner refused this device" }, "Not paired with Desk", "The request was refused on the computer. Nothing was paired, and that code no longer works."],
  [{ code: "expired", message: "The pairing code expired before it was confirmed on the computer" }, "The code expired", "Nobody confirmed this phone on Desk before the code expired, so nothing was paired."],
])("says calmly how a request ended unpaired and what to do next (%o)", async (error, title, lede) => {
  const held = heldPairing();
  await askToPair(held.platform);
  await act(async () => held.answer().reject(error));
  expect(screen.getByRole("heading", { name: title })).toBeTruthy();
  expect(screen.getByText(lede)).toBeTruthy();
  expect(screen.getByText(/To try again, run/).textContent).toBe("To try again, run ophio pair on the computer, scan the new code, then confirm on the computer when it asks.");
  // A code from an ended request is never left on screen to compare.
  expect(screen.queryByText("042 917")).toBeNull();
  expect(screen.getByRole("button", { name: "Use a new code" })).toBeTruthy();
});

it("withdraws the request when the person cancels, and the withdrawn request stays quiet", async () => {
  const held = heldPairing();
  await askToPair(held.platform);
  const withdrawn = held.answer();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Cancel" })); });
  expect(held.cancelPairing).toHaveBeenCalledOnce();
  expect(screen.getByRole("heading", { name: "Pair with Desk?" })).toBeTruthy();
  await act(async () => withdrawn.reject({ code: "temporarily_unavailable", message: "The connection closed before the computer confirmed the operation." }));
  expect(screen.queryByText("Not paired")).toBeNull();
  expect(screen.getByRole("button", { name: "Pair" })).toBeTruthy();
});

it("can be cancelled while the device is still reaching the computer, before any code is shown", async () => {
  const held = heldPairing({ reached: false });
  await askToPair(held.platform);
  expect(screen.queryByRole("heading", { name: "Check the code on Desk" })).toBeNull();
  expect(screen.getByRole("button", { name: "Pairing with Desk" })).toBeTruthy();
  const withdrawn = held.answer();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Cancel" })); });
  expect(held.cancelPairing).toHaveBeenCalledOnce();
  await act(async () => withdrawn.reject({ code: "temporarily_unavailable", message: "The connection closed before the computer confirmed the operation." }));
  expect(screen.queryByText("Not paired")).toBeNull();
  expect(screen.getByRole("button", { name: "Pair" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Use a new code" })).toBeTruthy();
});

it("withdraws the request when the person goes back while it waits", async () => {
  const held = heldPairing();
  await askToPair(held.platform);
  act(() => window.dispatchEvent(new PopStateEvent("popstate")));
  expect(screen.getByRole("heading", { name: "Pair with your computer" })).toBeTruthy();
  expect(held.cancelPairing).toHaveBeenCalledOnce();
});

it("says when another device is already asking the computer", async () => {
  const held = heldPairing();
  await askToPair(held.platform);
  await act(async () => held.answer().reject({ code: "conflict", message: "Another device is already waiting for approval on this computer" }));
  expect(screen.getByText("Not paired")).toBeTruthy();
  expect(screen.getByText(/Another device is already asking Desk to pair\. If it isn’t yours, refuse it on the computer\./)).toBeTruthy();
});

it("opens the confirmation from a link setup sent, says so, and keeps setup's alerts answer once paired", async () => {
  const held = heldPairing();
  const parsePairingLink = vi.fn(() => ({ offer: liveOffer(), alerts: false }));
  const platform = { ...held.platform, parsePairingLink } as unknown as Platform;
  localStorage.clear();
  Element.prototype.scrollTo ??= () => {};
  const link = { url: "fixture" };
  const view = render(<Welcome platform={platform} link={link} onPaired={() => {}} />);
  await act(async () => {});
  expect(screen.getByRole("heading", { name: "Pair with Desk?" })).toBeTruthy();
  expect(screen.getByText("A link opened this pairing")).toBeTruthy();
  // The same link, drawn again, is not taken twice.
  view.rerender(<Welcome platform={platform} link={link} onPaired={() => {}} />);
  expect(parsePairingLink).toHaveBeenCalledOnce();
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Pair" })); });
  expect(localStorage.getItem("alerts.preset")).toBeNull();
  const profile: HostProfile = { profileId: "p", hostId: "host", name: "Desk", pairedAt: 1, certSha256: "00".repeat(32), deviceId: "phone", addresses: ["127.0.0.1"] };
  await act(async () => held.answer().resolve({ profile, response: { device_id: "phone", host_id: "host", host_name: "Desk", grants: {} } }));
  expect(screen.getByRole("heading", { name: "Paired with Desk" })).toBeTruthy();
  expect(localStorage.getItem("alerts.preset")).toBe("off");
});

it("does not say a link opened a pairing the person pasted", async () => {
  const { pair } = await confirmWith(info());
  expect(pair).not.toHaveBeenCalled();
  expect(screen.queryByText("A link opened this pairing")).toBeNull();
});
