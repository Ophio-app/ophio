// SPDX-License-Identifier: Apache-2.0
export interface DecodedImage {
  image: CanvasImageSource;
  width: number;
  height: number;
  close(): void;
}

/** The decoder used by image previews and the drawing surface. Keep the URL
 * alive until the caller has finished measuring or drawing the pixels. */
export function loadImage(blob: Blob): Promise<DecodedImage> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    const url = URL.createObjectURL(blob);
    let closed = false;
    const close = () => {
      if (closed) return;
      closed = true;
      clearTimeout(timer);
      image.onload = image.onerror = null;
      image.removeAttribute("src");
      URL.revokeObjectURL(url);
    };
    const fail = () => {
      close();
      reject(new Error("This image couldn’t be opened."));
    };
    const timer = setTimeout(fail, 15000);
    image.onload = () => {
      if (!image.naturalWidth || !image.naturalHeight) return fail();
      clearTimeout(timer);
      image.onload = image.onerror = null;
      resolve({ image, width: image.naturalWidth, height: image.naturalHeight, close });
    };
    image.onerror = fail;
    image.src = url;
  });
}

/** Bitmap decoding is efficient for photos, but it rejects some images that
 * the drawing itself can display (notably SVG blobs in Chromium). Use that
 * same image-element decoder before declaring the source unreadable. */
export async function decodeImage(blob: Blob): Promise<DecodedImage> {
  try {
    const image = await createImageBitmap(blob);
    if (image.width && image.height) {
      return { image, width: image.width, height: image.height, close: () => image.close() };
    }
    image.close();
  } catch {
    // A missing bitmap API or unsupported bitmap format is not a corrupt file.
  }
  return loadImage(blob);
}
