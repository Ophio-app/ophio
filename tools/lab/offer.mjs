// SPDX-License-Identifier: Apache-2.0
// Where the emulator reaches a lab guest, from the guest's process.json.
export function guestAddress(record) {
  const port = record?.forwards?.ophio;
  if (!(Number.isInteger(port) && port >= 1024 && port <= 65535))
    throw new Error("The lab guest's process.json has no Øphio forward");
  // The emulator reaches the host's loopback through 10.0.2.2.
  return `10.0.2.2:${port}`;
}
