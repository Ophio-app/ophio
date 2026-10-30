// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { PRODUCT_NAME } from "../../brand";
import { download } from "../../host/transfer";
import type { AppOffer } from "../../protocol/types";
import { bytes } from "../library/model";

type Step =
  | { step: "idle" }
  | { step: "downloading"; received: number; size: number }
  | { step: "installing" }
  | { step: "asking" }
  | { step: "failed"; message: string };

/** What this app made of the computer's offers: the one it can update to, if any. */
export interface AppOfferFound { offer: AppOffer | null; asks: boolean }

/** The app update the computer offers this device, once the device has checked it can take it. */
export function useAppOffer(enabled = true): AppOfferFound | null {
  const { platform } = useApp();
  const offers = useHost((s) => s.capabilities?.app_updates);
  const updater = platform.appUpdate;
  const [found, setFound] = useState<AppOfferFound | null>(null);
  useEffect(() => {
    if (!enabled || !updater || !offers?.length) { setFound(null); return; }
    let alive = true;
    updater.check(offers)
      .then((r) => { if (alive) setFound({ offer: r.offer, asks: r.asks }); })
      .catch(() => { if (alive) setFound(null); });
    return () => { alive = false; };
  }, [enabled, updater, offers]);
  return found;
}

/**
 * Updates this app to the one the computer offers, over the paired
 * connection, so it needs no cable, store or wireless debugging. Android
 * installs only an app signed as this one is. A caller that has already
 * checked passes what it found.
 */
export function AppUpdate({ found: given }: { found?: AppOfferFound } = {}) {
  const { platform, profile, session } = useApp();
  const link = useLink();
  const offers = useHost((s) => s.capabilities?.app_updates);
  const transfers = useHost((s) => s.grants?.transfers ?? false);
  const updater = platform.appUpdate;
  const own = useAppOffer(given === undefined);
  const found = given ?? own;
  const [step, setStep] = useState<Step>({ step: "idle" });
  useEffect(() => updater?.onResult((r) => {
    if (r.state === "asking") setStep({ step: "asking" });
    else if (r.state === "failed") setStep({ step: "failed", message: r.message ?? "Android couldn’t install the update." });
  }), [updater]);
  if (!updater) return null;
  const offer = found?.offer;
  const online = link.state === "online";
  const busy = step.step === "downloading" || step.step === "installing";
  const update = async () => {
    if (!offer) return;
    setStep({ step: "downloading", received: 0, size: offer.size });
    try {
      const got = await download({ platform, profileId: profile.profileId, session }, {
        source: { kind: "app_update", sha256: offer.sha256 },
        label: "Update the phone app",
        onProgress: (received, size) => setStep({ step: "downloading", received, size: size || offer.size }),
      });
      if (got.sha256 !== offer.sha256) throw new Error("The computer sent a different app. Check for an update again.");
      setStep({ step: "installing" });
      announce("Installing the update. The app closes and reopens.");
      await updater.install(profile.profileId, got.blob, got.sha256);
    } catch (e) {
      // Android's own refusals arrive as plain text.
      setStep({ step: "failed", message: typeof e === "string" ? e : describeError(e, profile.name) });
    }
  };
  return <div className="settings-stack">
    {!offers?.length && <p className="settings-meta">
      {profile.name} doesn’t offer app updates. Update {PRODUCT_NAME} on it first; its new version brings the app for this phone.
    </p>}
    {offers?.length && found && !offer && <p className="settings-meta">This app is up to date with {profile.name}.</p>}
    {offer && <>
      <p>{profile.name} has {PRODUCT_NAME} {offer.version} for this phone ({bytes(offer.size)}).</p>
      <p className="settings-meta">
        {found?.asks ? "Android asks you to confirm the update." : "Android installs it without asking."} The app closes
        while it installs; open it again from the notification. The download goes straight to Android, so nothing is left behind.
      </p>
      {step.step === "downloading" && <>
        <p className="settings-meta num" role="status">Downloading · {bytes(step.received)} of {bytes(step.size)}</p>
        <progress aria-label="Update downloaded" value={step.received} max={step.size} />
      </>}
      {step.step === "installing" && <p className="settings-meta" role="status">Installing…</p>}
      {step.step === "asking" && <p className="settings-meta" role="status">Confirm the update in Android’s prompt.</p>}
      {step.step === "failed" && <Notice tone="warn">{step.message}</Notice>}
      {!transfers && <p className="settings-meta">This device needs the File transfers permission to download the update.</p>}
      {!online && <Notice tone="unknown">Offline. Connect to update.</Notice>}
      <Button variant="primary" pending={busy} disabled={busy || !online || !transfers} onClick={() => void update()}>
        {step.step === "failed" || step.step === "asking" ? "Try again" : "Update now"}
      </Button>
    </>}
  </div>;
}
