// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { decodeImage, loadImage } from "./images";

let images: FakeImage[];
class FakeImage {
  naturalWidth = 32;
  naturalHeight = 20;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  src = "";
  removeAttribute = vi.fn();
  constructor() { images.push(this); }
}
beforeEach(() => {
  images = [];
  vi.useFakeTimers();
  vi.stubGlobal("Image", FakeImage);
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:test");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  vi.stubGlobal("createImageBitmap", vi.fn().mockRejectedValue(new Error("unsupported bitmap format")));
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
const blob = () => new Blob(["image"], { type: "image/svg+xml" });

it("keeps bitmap decoding and closes its resource", async () => {
  const bitmap = { width: 20, height: 32, close: vi.fn() };
  vi.mocked(createImageBitmap).mockResolvedValue(bitmap as unknown as ImageBitmap);
  const decoded = await decodeImage(blob());
  expect(decoded).toMatchObject({ width: 20, height: 32, image: bitmap });
  decoded.close();
  expect(bitmap.close).toHaveBeenCalledOnce();
  expect(images).toHaveLength(0);
});

it("uses the display decoder if bitmap decoding rejects a valid image", async () => {
  const pending = decodeImage(blob());
  await Promise.resolve();
  images[0]!.onload!();
  const decoded = await pending;
  expect(decoded).toMatchObject({ width: 32, height: 20, image: images[0] });
  expect(URL.revokeObjectURL).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
  decoded.close();
  decoded.close();
  expect(URL.revokeObjectURL).toHaveBeenCalledExactlyOnceWith("blob:test");
  expect(images[0]!.removeAttribute).toHaveBeenCalledWith("src");
});

it("works when the bitmap API is unavailable", async () => {
  vi.stubGlobal("createImageBitmap", undefined);
  const pending = decodeImage(blob());
  images[0]!.onload!();
  const decoded = await pending;
  expect(decoded.width).toBe(32);
  decoded.close();
});

it("rejects undecodable images and releases the URL", async () => {
  const pending = decodeImage(blob());
  const rejected = expect(pending).rejects.toThrow("couldn’t be opened");
  await Promise.resolve();
  images[0]!.onerror!();
  await rejected;
  expect(URL.revokeObjectURL).toHaveBeenCalledExactlyOnceWith("blob:test");
  expect(vi.getTimerCount()).toBe(0);
});

it("rejects images with no usable dimensions", async () => {
  const pending = loadImage(blob());
  images[0]!.naturalWidth = 0;
  images[0]!.onload!();
  await expect(pending).rejects.toThrow("couldn’t be opened");
  expect(URL.revokeObjectURL).toHaveBeenCalledOnce();
});

it("releases a stalled image after a bounded wait and ignores late events", async () => {
  const pending = loadImage(blob());
  const rejected = expect(pending).rejects.toThrow("couldn’t be opened");
  await vi.advanceTimersByTimeAsync(15000);
  await rejected;
  expect(images[0]!.onload).toBeNull();
  expect(images[0]!.onerror).toBeNull();
  expect(URL.revokeObjectURL).toHaveBeenCalledOnce();
  expect(vi.getTimerCount()).toBe(0);
});
