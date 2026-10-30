// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { useEffect, useState } from "react";
import { useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { reportPlatformError } from "../../platform/errors";
import { useStore } from "../../state/store";
import { AppUpdate } from "./AppUpdate";
import { compatibility, installerWords, versionText } from "./compatibility";
import packageInfo from "../../../package.json";

export function protectionWords(value: string | undefined): string {
  if (value === "keystore") return "Device key kept in Android’s secure storage";
  if (value === "wrapped")
    return "Device key locked by a key kept in Android’s secure storage";
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
  return (
    <Section title="Versions" id="settings-versions">
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
          <div>
            <dt>App protocol</dt>
            <dd>{versionText(info?.protocol)}</dd>
          </div>
          <div>
            <dt>Negotiated protocol</dt>
            <dd>{versionText(negotiated)}</dd>
          </div>
        </dl>
        <Notice tone={link.state === "failed" && link.reason === "incompatible_version" ? "warn" : "info"}>{compatibility(info?.protocol ?? null, negotiated, link.state === "failed" && link.reason === "incompatible_version", link.state === "failed" ? link.detail : undefined)}</Notice>
        <p>{protectionWords(profile.keyProtection ?? info?.keyProtection)}</p>
        <p className="settings-meta">
          {installerWords(info?.installerPackage, !!platform.appUpdate)}
        </p>
        <AppUpdate />
        <Button onClick={() => setNotices(true)}>Open-source notices</Button>
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
