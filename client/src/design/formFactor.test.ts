// SPDX-License-Identifier: Apache-2.0
import { afterEach, describe, expect, it, vi } from "vitest";
import { protectionWords } from "../screens/settings/Versions";
import { deviceKind, onIPhone } from "./formFactor";

const IPHONE_UA = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148";
const ANDROID_UA = "Mozilla/5.0 (Linux; Android 16; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Mobile Safari/537.36";

afterEach(() => vi.restoreAllMocks());
const as = (ua: string) => vi.spyOn(navigator, "userAgent", "get").mockReturnValue(ua);

describe("the phone in hand", () => {
  it("tells an iPhone from an Android phone", () => {
    as(IPHONE_UA);
    expect(onIPhone()).toBe(true);
    expect(deviceKind()).toBe("phone");
    as(ANDROID_UA);
    expect(onIPhone()).toBe(false);
  });

  it("names the key's protection for the phone's own vault", () => {
    as(IPHONE_UA);
    expect(protectionWords("wrapped")).toBe("Device key sealed by the iPhone’s Secure Enclave");
    as(ANDROID_UA);
    expect(protectionWords("wrapped")).toBe("Device key locked by a key kept in Android’s secure storage");
    expect(protectionWords("keystore")).toBe("Device key kept in Android’s secure storage");
  });
});
