// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef } from "react";
import type { Platform } from "../host/link";
import { ShareIntake } from "./shares";
import { reportPlatformError } from "./errors";
import { parseOpenRoute, type OpenRoute } from "./notifications";

const intakes = new WeakMap<Platform, ShareIntake>();
/** Mounted above pairing and connected views, so subscriptions survive navigation. */
export function PlatformEvents({
  platform,
  profileId,
  onOpen,
}: {
  platform: Platform;
  profileId: string | null;
  onOpen: (route: OpenRoute) => void;
}) {
  const current = useRef({ profileId, onOpen });
  current.current = { profileId, onOpen };
  let intake = intakes.get(platform);
  if (!intake) {
    intake = new ShareIntake();
    intakes.set(platform, intake);
  }
  useEffect(() => {
    const offErrors = platform.onPlatformError?.(reportPlatformError);
    const offShares = platform.onShared?.((items) =>
      intake.receive(items, current.current.profileId),
    );
    const offOpen = platform.onNotificationOpen?.((notification) => {
      // One that leads nowhere, as the count of alerts waiting for room,
      // only opens the app.
      if (notification.route == null) return;
      const route = parseOpenRoute(notification.route);
      if (route) current.current.onOpen(route);
      else
        reportPlatformError({
          operation: "Open notification",
          message: "This notification has no usable item reference.",
        });
    });
    return () => {
      offErrors?.();
      offShares?.();
      offOpen?.();
    };
  }, [platform, intake]);
  useEffect(() => {
    if (profileId) void intake.paired(profileId);
  }, [intake, profileId]);
  return null;
}
