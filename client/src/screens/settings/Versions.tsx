// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { useEffect, useState } from "react";
import { useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { onIPhone } from "../../design/formFactor";
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { reportPlatformError } from "../../platform/errors";
import { useStore } from "../../state/store";
import { AppUpdate, useAppOffer } from "./AppUpdate";
import { NewVersion } from "./NewVersion";
import { compatibility, installerWords, versionText } from "./compatibility";
import packageInfo from "../../../package.json";

export function protectionWords(value: string | undefined): string {
  if (value === "keystore") return "Device key kept in Android’s secure storage";
  if (value === "wrapped")
    return onIPhone()
      ? "Device key sealed by the iPhone’s Secure Enclave"
      : "Device key locked by a key kept in Android’s secure storage";
  if (value === "simulated")
    return "Simulated device key — no Android key is stored";
  return "Device key protection is not reported by this build";
}
/** Packaging copies the notices into the app's web bundle under `licenses/`. */
const NOTICE_FILES = ["NOTICE", "THIRD-PARTY-NOTICES.txt"];

/** The bundled notices, or null when this build does not include them. */
export async function loadNotices(
  fetcher: typeof fetch = fetch,
  base: string = import.meta.env.BASE_URL,
): Promise<string | null> {
  const texts = [];
  for (const name of NOTICE_FILES) {
    const res = await fetcher(`${base}licenses/${name}`);
    // A development server answers any path with the app's page.
    if (!res.ok || /html/i.test(res.headers.get("content-type") ?? ""))
      return null;
    texts.push((await res.text()).trim());
  }
  return texts.join("\n\n");
}

function Notices() {
  const [text, setText] = useState<string | null | undefined>(undefined);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    loadNotices()
      .then((v) => {
        if (alive) setText(v);
      })
      .catch(() => {
        if (alive) setFailed(true);
      });
    return () => {
      alive = false;
    };
  }, []);
  if (failed)
    return (
      <Notice tone="warn">
        The notices couldn’t be read from this app. They are also in the
        release’s THIRD-PARTY-NOTICES.txt.
      </Notice>
    );
  if (text === undefined) return <p className="settings-meta">Loading…</p>;
  if (text === null)
    return (
      <p>
        This development build doesn’t include the notices. Release builds
        of {PRODUCT_NAME} for phones and computers carry them, with the
        Apache-2.0 licence.
      </p>
    );
  return <pre className="settings-notices">{text}</pre>;
}

export function Versions() {
  const { platform, profile, session } = useApp();
  const negotiated = useStore(session.negotiated, (s) => s);
  const link = useLink();
  const host = useHost((s) => s.host);
  const [notices, setNotices] = useState(false);
  // One check for the page: where the update sits depends on what it finds.
  const found = useAppOffer();
  const offered = !!found?.offer;
  const openNotices = (
    <Button onClick={() => setNotices(true)}>Open-source notices</Button>
  );
  const [info, setInfo] = useState<Awaited<
    ReturnType<NonNullable<typeof platform.appInfo>>
  > | null>(null);
  useEffect(() => {
    let alive = true;
    void platform
      .appInfo?.()
      .then((v) => {
        if (alive) setInfo(v);
      })
      .catch((error: unknown) => {
        if (alive)
          reportPlatformError({
            operation: "App information",
            message:
              error instanceof Error
                ? error.message
                : "This device couldn’t read app information.",
          });
      });
    return () => {
      alive = false;
    };
  }, [platform, profile.profileId]);
  const incompatible = link.state === "failed" && link.reason === "incompatible_version";
  const app = info?.protocol ?? null;
  // Anything that asks for an update stays in view.
  const needsAction = incompatible || (!!app && !!negotiated && (negotiated.major !== app.major || negotiated.minor < app.minor));
  const compatibilityNotice = <Notice tone={incompatible ? "warn" : "info"}>{compatibility(app, negotiated, incompatible, link.state === "failed" ? link.detail : undefined)}</Notice>;
  return (
    <Section title="Versions" id="settings-versions">
      {/* A newer app for this phone leads, with its Update now and the notices
          button under it: neither is ever pushed below the first screen. */}
      {offered && (
        <div className="settings-card settings-card--update settings-stack">
          <AppUpdate found={found} />
          {openNotices}
        </div>
      )}
      {/* A notification about a newer version for the computer opens here. */}
      <NewVersion />
      <div className="settings-card settings-stack">
        <dl className="settings-facts">
          <div>
            <dt>App</dt>
            <dd>{info?.version ?? packageInfo.version}</dd>
          </div>
          <div>
            <dt>Build</dt>
            <dd>
              {info?.build ??
                (platform.appInfo ? "Not reported yet" : "Browser build")}
            </dd>
          </div>
          <div>
            <dt>Computer</dt>
            <dd>{host?.companion_version ?? "Unknown"}</dd>
          </div>
        </dl>
        {needsAction && compatibilityNotice}
        <p>{protectionWords(profile.keyProtection ?? info?.keyProtection)}</p>
        <p className="settings-meta">
          {installerWords(info?.installerPackage, !!platform.appUpdate)}
        </p>
        {/* Protocol numbers are for support; they come forward only when
            the two sides can't work together fully. */}
        <details className="settings-support">
          <summary>Details for support</summary>
          <dl className="settings-facts">
            <div>
              <dt>App protocol</dt>
              <dd>{versionText(info?.protocol)}</dd>
            </div>
            <div>
              <dt>Negotiated protocol</dt>
              <dd>{versionText(negotiated)}</dd>
            </div>
          </dl>
          {!needsAction && compatibilityNotice}
        </details>
        {!offered && (
          <>
            <AppUpdate found={found} />
            {openNotices}
          </>
        )}
      </div>
      <Sheet
        open={notices}
        onClose={() => setNotices(false)}
        title="Open-source notices"
        subtitle="This app’s licence and the software it includes"
        size="full"
      >
        {notices && <Notices />}
      </Sheet>
    </Section>
  );
}
