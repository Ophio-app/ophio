// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { DeviceRecord, Grants } from "../../protocol/types";
import { REVIEW_AFTER_MS, notSeenSince } from "./deviceActivity";
const grants: Grants = { tasks: true, approvals: false, view: false, control_input: false,
  computer_use: false, terminal: false, library_read: false, library_write: false, transfers: false, admin: false, accounts: false };
const minute = 60_000;
const old: DeviceRecord = { device_id: "old", name: "Tablet", platform: "android", paired_at: 10,
  last_seen_at: null, revoked_at: null, is_current: false, grants };
const recent = { ...old, device_id: "new", paired_at: 20 };
const later = 10 + REVIEW_AFTER_MS;

it("marks long observation gaps without assuming matching device names mean the same install", () => {
  expect(notSeenSince(old, [old, recent], later)).toBe(10);
  expect(notSeenSince({ ...old, last_seen_at: 19 }, [old, { ...recent, name: "Different name" }], later + 19)).toBe(19);
  expect(notSeenSince({ ...old, last_seen_at: 20 }, [old, recent], later + 20)).toBeNull();
  expect(notSeenSince({ ...old, revoked_at: 21 }, [old, recent], later)).toBeNull();
  expect(notSeenSince({ ...old, is_current: true }, [old, recent], later)).toBeNull();
  expect(notSeenSince(recent, [old, recent], later + 20)).toBeNull();
  expect(notSeenSince(old, [old], later)).toBeNull();
});

it("does not suggest revoking a phone because a tablet paired minutes after it", () => {
  const phone = { ...old, device_id: "phone", paired_at: 0, last_seen_at: 100 * minute };
  const tablet = { ...old, device_id: "tablet", paired_at: 105 * minute, last_seen_at: 106 * minute };
  expect(notSeenSince(phone, [phone, tablet], 110 * minute)).toBeNull();
  expect(notSeenSince(phone, [phone, tablet], 100 * minute + REVIEW_AFTER_MS - 1)).toBeNull();
  expect(notSeenSince(phone, [phone, tablet], 100 * minute + REVIEW_AFTER_MS)).toBe(100 * minute);
});
