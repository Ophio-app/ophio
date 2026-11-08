// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { PRODUCT_NAME } from "../../brand";
import { download } from "../../host/transfer";
import type { AppUpdater } from "../../host/link";
import type { AppOffer } from "../../protocol/types";
import { bytes } from "../library/model";

type Step =
  | { step: "idle" }
  /** Asking Android what it allows, before anything is downloaded. */
  | { step: "checking" }
  /** Android's Install unknown apps setting is open; the update carries on on return. */
  | { step: "allowing" }
  /** Back from that setting, and the app still may not install apps. */
  | { step: "refused" }
  | { step: "downloading"; received: number; size: number }
  | { step: "installing" }
  | { step: "asking" }
  | { step: "failed"; message: string };

/**
 * What this app made of the computer's offers: the one it can update to, if
 * any; whether Android lets the app install apps; and whether Android asks the
 * person to confirm the update even then.
 */
export interface AppOfferFound { offer: AppOffer | null; allowed: boolean; asks: boolean }

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
      .then((r) => { if (alive) setFound({ offer: r.offer, allowed: r.allowed, asks: r.asks }); })
      .catch(() => { if (alive) setFound(null); });
    return () => { alive = false; };
  }, [enabled, updater, offers]);
  return found;
}

/**
 * Updates this app to the one the computer offers, over the paired
 * connection, so it needs no cable, store or wireless debugging. Android
 * installs only an app signed as this one is, and only once the person lets
 * this app install apps. Android stops an install it may not make yet to send
 * the person to that setting, and can cancel it on the way back; so the
 * setting comes first, and the update carries on when the app is back in
 * front. A caller that has already checked passes what it found.
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
  /** Android's answer since the person asked to update, newer than `found`'s. */
  const [fresh, setFresh] = useState<{ allowed: boolean; asks: boolean } | null>(null);
  const again = useRef<HTMLButtonElement>(null);
  // Back from Android's setting the warning can sit below the fold: bring it
  // and the button under it into view.
  useEffect(() => {
    if (step.step === "refused" || step.step === "failed") again.current?.scrollIntoView?.({ block: "nearest" });
  }, [step.step]);
  useEffect(() => updater?.onResult((r) => {
    if (r.state === "asking") setStep({ step: "asking" });
    else if (r.state === "failed") setStep({ step: "failed", message: r.message ?? "Android couldn’t install the update." });
  }), [updater]);
  const offer = found?.offer;
  const failed = (e: unknown): Step =>
    // Android's own refusals arrive as plain text.
    ({ step: "failed", message: typeof e === "string" ? e : describeError(e, profile.name) });
  const fetchAndInstall = async (offer: AppOffer, updater: AppUpdater) => {
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
      setStep(failed(e));
    }
  };
  // Back from Android's setting: ask again, and carry on if it is allowed now.
  const allowing = step.step === "allowing";
  useEffect(() => {
    if (!allowing || !updater || !offer || !offers?.length) return;
    let waiting = true;
    const off = platform.onForeground?.(() => {
      if (!waiting) return;
      waiting = false;
      setStep({ step: "checking" });
      updater.check(offers).then((now) => {
        setFresh({ allowed: now.allowed, asks: now.asks });
        if (now.allowed) void fetchAndInstall(offer, updater);
        else setStep({ step: "refused" });
      }, (e: unknown) => setStep(failed(e)));
    });
    return () => { waiting = false; off?.(); };
    // Only entering and leaving the wait matters; the rest is read then.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allowing]);
  if (!updater) return null;
  const online = link.state === "online";
  const busy = step.step === "checking" || step.step === "downloading" || step.step === "installing";
  const allowed = fresh?.allowed ?? found?.allowed ?? true;
  const asks = fresh?.asks ?? found?.asks ?? true;
  const update = async () => {
    if (!offer || !offers?.length) return;
    setStep({ step: "checking" });
    try {
      // Opens Android's setting unless the app may already install apps.
      const now = await updater.check(offers, { openSettings: true });
      setFresh({ allowed: now.allowed, asks: now.asks });
      if (!now.allowed) { setStep({ step: "allowing" }); return; }
    } catch (e) {
      setStep(failed(e));
      return;
    }
    await fetchAndInstall(offer, updater);
  };
  return <div className="settings-stack">
    {!offers?.length && <p className="settings-meta">
      {profile.name} doesn’t offer app updates. Update {PRODUCT_NAME} on it first; its new version brings the app for this phone.
    </p>}
    {offers?.length && found && !offer && <p className="settings-meta">This app is up to date with {profile.name}.</p>}
    {offer && <>
      <p>{profile.name} has {PRODUCT_NAME} {offer.version} for this phone ({bytes(offer.size)}).</p>
      <p className="settings-meta">
        {asking(allowed, asks)} The app closes while it installs; open it again from the notification. The download goes
        straight to Android, so nothing is left behind.
      </p>
      {step.step === "downloading" && <>
        <p className="settings-meta num" role="status">Downloading · {bytes(step.received)} of {bytes(step.size)}</p>
        <progress aria-label="Update downloaded" value={step.received} max={step.size} />
      </>}
      {step.step === "checking" && <p className="settings-meta" role="status">Checking with Android…</p>}
      {step.step === "allowing" && <p className="settings-meta" role="status">
        Turn on Allow from this source for {PRODUCT_NAME} in Android’s settings, then come back here.
      </p>}
      {step.step === "refused" && <Notice tone="warn">
        {PRODUCT_NAME} still isn’t allowed to install apps, so the update hasn’t started. Tap Update now to open
        Android’s setting again and turn on Allow from this source.
      </Notice>}
      {step.step === "installing" && <p className="settings-meta" role="status">Installing…</p>}
      {step.step === "asking" && <p className="settings-meta" role="status">Confirm the update in Android’s prompt.</p>}
      {step.step === "failed" && <Notice tone="warn">{step.message}</Notice>}
      {!transfers && <p className="settings-meta">This device needs the File transfers permission to download the update.</p>}
      {!online && <Notice tone="unknown">Offline. Connect to update.</Notice>}
      <Button ref={again} variant="primary" pending={busy} disabled={busy || !online || !transfers} onClick={() => void update()}>
        {step.step === "failed" || step.step === "asking" ? "Try again" : "Update now"}
      </Button>
    </>}
  </div>;
}

/** What Android asks before it installs the update, never more certain than Android is. */
function asking(allowed: boolean, asks: boolean): string {
  if (!allowed)
    return `Android first needs you to allow ${PRODUCT_NAME} to install apps: Update now opens that setting, and the update `
      + `carries on when you come back.${asks ? " Android then asks you to confirm the update." : ""}`;
  // From Android 12 an app may update itself unasked, but Android still asks
  // when one update follows another within half a minute.
  return asks ? "Android asks you to confirm the update." : "Android usually installs it without asking.";
}
