// SPDX-License-Identifier: Apache-2.0
/** Minimal finite WebM writer for the VP8 chunks produced by our encoder.
 * No demuxing, arbitrary metadata, or audio tracks are accepted. */
export interface VideoPacket {
  timestamp: number;
  duration: number;
  key: boolean;
  bytes: Uint8Array<ArrayBuffer>;
}
const number = (value: number): Uint8Array<ArrayBuffer> => {
  const bytes = [];
  do {
    bytes.unshift(value % 256);
    value = Math.floor(value / 256);
  } while (value);
  return new Uint8Array(bytes);
};
const size = (value: number): Uint8Array<ArrayBuffer> => {
  for (let n = 1; n <= 7; n++) {
    if (value < 2 ** (7 * n) - 1) {
      const result = new Uint8Array(n);
      for (let i = n - 1; i >= 0; i--) {
        result[i] = value % 256;
        value = Math.floor(value / 256);
      }
      result[0] = (result[0] ?? 0) | (1 << (8 - n));
      return result;
    }
  }
  throw new Error("The encoded clip is too large.");
};
const element = (id: number, body: Blob | Uint8Array<ArrayBuffer>): Blob =>
  new Blob([
    number(id),
    size(body instanceof Blob ? body.size : body.byteLength),
    body,
  ]);
const uint = (id: number, value: number) => element(id, number(value));
const string = (id: number, value: string) =>
  element(id, new TextEncoder().encode(value));
const float = (id: number, value: number) => {
  const data = new Uint8Array(8);
  new DataView(data.buffer).setFloat64(0, value);
  return element(id, data);
};
export function webm(
  packets: VideoPacket[],
  width: number,
  height: number,
  durationMs: number,
): Blob {
  if (
    !packets.length ||
    !packets[0]?.key ||
    packets[0].timestamp !== 0 ||
    !Number.isFinite(durationMs) ||
    durationMs <= 0
  )
    throw new Error("The encoder did not produce a complete clip.");
  if (
    ![width, height, durationMs].every(Number.isSafeInteger) ||
    width <= 0 ||
    height <= 0
  )
    throw new Error(
      "The encoder returned invalid video dimensions or duration.",
    );
  let previous = -1;
  let expectedTime = 0;
  for (const packet of packets) {
    if (
      !packet.bytes.length ||
      packet.timestamp <= previous ||
      packet.timestamp !== expectedTime ||
      packet.duration <= 0 ||
      packet.timestamp + packet.duration > durationMs + 0.01
    )
      throw new Error("The encoder returned invalid frame timing.");
    previous = packet.timestamp;
    expectedTime = packet.timestamp + packet.duration;
  }
  if (expectedTime !== durationMs)
    throw new Error("The encoder did not finish the selected range.");
  const header = element(
    0x1a45dfa3,
    new Blob([
      uint(0x4286, 1),
      uint(0x42f7, 1),
      uint(0x42f2, 4),
      uint(0x42f3, 8),
      string(0x4282, "webm"),
      uint(0x4287, 4),
      uint(0x4285, 2),
    ]),
  );
  const info = element(
    0x1549a966,
    new Blob([
      uint(0x2ad7b1, 1_000_000),
      float(0x4489, durationMs),
      string(0x4d80, "Clip export"),
      string(0x5741, "Clip export"),
    ]),
  );
  const tracks = element(
    0x1654ae6b,
    element(
      0xae,
      new Blob([
        uint(0xd7, 1),
        uint(0x73c5, 1),
        uint(0x83, 1),
        uint(0x9c, 0),
        string(0x86, "V_VP8"),
        element(0xe0, new Blob([uint(0xb0, width), uint(0xba, height)])),
      ]),
    ),
  );
  const clusters = packets.map((p, i) => {
    const reference = p.key
      ? []
      : [
          element(
            0xfb,
            new Uint8Array([
              0xff,
              0xff,
              (((packets[i - 1]?.timestamp ?? 0) - p.timestamp) >> 8) & 255,
              ((packets[i - 1]?.timestamp ?? 0) - p.timestamp) & 255,
            ]),
          ),
        ];
    return element(
      0x1f43b675,
      new Blob([
        uint(0xe7, p.timestamp),
        element(
          0xa0,
          new Blob([
            element(0xa1, new Blob([new Uint8Array([0x81, 0, 0, 0]), p.bytes])),
            uint(0x9b, p.duration),
            ...reference,
          ]),
        ),
      ]),
    );
  });
  return new Blob(
    [header, element(0x18538067, new Blob([info, tracks, ...clusters]))],
    { type: "video/webm" },
  );
}
