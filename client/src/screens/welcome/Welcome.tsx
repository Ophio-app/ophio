// SPDX-License-Identifier: Apache-2.0
//
// Welcome and computer pairing. The computer shows a short-lived code
// only when its owner runs the pair command there, which is the local
// approval. A failed attempt leaves this device unpaired; the grants shown at
// the end are the ones the computer returned, not a promise made here.

import {
  ArrowLeft,
  Camera,
  Check,
  ClipboardPaste,
  Laptop,
  Minus,
  MonitorSmartphone,
  Network,
  ShieldCheck,
} from "lucide-react";
import { useEffect, useRef, useState, type RefObject } from "react";
import { PROGRAM_NAME, PAIRING_SCHEME, PRODUCT_NAME } from "../../brand";
import { announce } from "../../design/Announcer";
import { useBackLayer } from "../../design/back";
import { deviceKind } from "../../design/formFactor";
import { Button, IconButton } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { useCrampedLayout, useKeyboardOpen } from "../../shell/keyboard";
import { countdown, useExpiry } from "./countdown";
import type { HostProfile, PairingResult, Platform } from "../../host/link";
import { fingerprintWords, type PairingLinkProblem } from "../../protocol/pairing";
import type { Grants, PairingOffer } from "../../protocol/types";

import { alertsPreset } from "../../platform/alerts";
import { CompanionDownload } from "./CompanionDownload";
import { InlineCode } from "../../design/InlineCode";

/** A pairing link another app opened this one with, such as `ophio setup`
 * over Wireless debugging. Each one is taken once. */
export interface OpenedLink { url: string }

type Step =
  | { step: "intro" }
  | { step: "connect"; paste: boolean; link?: OpenedLink }
  | { step: "confirm"; offer: PairingOffer; alerts?: boolean; linked?: boolean }
  | { step: "done"; result: PairingResult };

const RESUME_KEY = "welcome.step";

export function Welcome({ platform, link, onPaired, onCancel }: {
  platform: Platform; link?: OpenedLink | null; onPaired: (p: HostProfile) => void; onCancel?: () => void;
}) {
  const [s, setS] = useState<Step>(() =>
    onCancel || localStorage.getItem(RESUME_KEY) === "connect" ? { step: "connect", paste: false } : { step: "intro" },
  );
  const headingRef = useRef<HTMLHeadingElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const keyboard = useKeyboardOpen();
  const cramped = useCrampedLayout();
  useEffect(() => { if (link) setS({ step: "connect", paste: false, link }); }, [link]);

  useEffect(() => {
    if (s.step === "connect") localStorage.setItem(RESUME_KEY, "connect");
    headingRef.current?.focus({ preventScroll: true });
    root.current?.scrollTo({ top: 0 });
  }, [s.step]);

  // The pinned actions cover the end of the page. Whatever is scrolled to,
  // a focused field included, lands above them (see welcome.css).
  useEffect(() => {
    const page = root.current;
    const actions = page?.querySelector<HTMLElement>(".welcome__actions");
    if (!page || !actions || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => page.style.setProperty("--welcome-actions", `${actions.offsetHeight}px`));
    observer.observe(actions, { box: "border-box" });
    return () => observer.disconnect();
  }, [s]);

  const back =
    s.step === "confirm" ? () => setS({ step: "connect", paste: false }) : s.step === "connect" && !onCancel ? () => setS({ step: "intro" }) : onCancel;
  // System back follows the on-screen Back arrow instead of leaving the app mid-pairing.
  useBackLayer(Boolean(back) && s.step !== "done", () => back?.());

  return (
    <div ref={root} className={`welcome${s.step === "intro" ? " welcome--intro" : ""}${keyboard && cramped ? " welcome--typing" : ""}`}>
      <div className="welcome__bar">
        {back && s.step !== "done" && <IconButton icon={ArrowLeft} label="Back" onClick={back} />}
      </div>
      <div className="welcome__inner">
        {s.step === "intro" && <Intro platform={platform} headingRef={headingRef} onNext={() => setS({ step: "connect", paste: false })} />}
        {s.step === "connect" && (
          <Connect
            headingRef={headingRef}
            platform={platform}
            paste={s.paste}
            link={s.link}
            setPaste={(paste) => setS({ step: "connect", paste })}
            onOffer={(offer, alerts, linked) => setS({ step: "confirm", offer, alerts, linked })}
          />
        )}
        {s.step === "confirm" && (
          <Confirm
            headingRef={headingRef}
            platform={platform}
            offer={s.offer}
            linked={s.linked}
            onRescan={() => setS({ step: "connect", paste: false })}
            onDone={(result) => {
              localStorage.removeItem(RESUME_KEY);
              if (s.alerts !== undefined) alertsPreset.set(s.alerts);
              platform.haptic?.("confirm");
              announce(`Paired with ${result.profile.name}`);
              setS({ step: "done", result });
            }}
          />
        )}
        {s.step === "done" && <Done headingRef={headingRef} result={s.result} onContinue={() => onPaired(s.result.profile)} />}
      </div>
    </div>
  );
}

type HeadingRef = RefObject<HTMLHeadingElement | null>;

function Intro({ platform, headingRef, onNext }: { platform: Platform; headingRef: HeadingRef; onNext: () => void }) {
  const kind = deviceKind();
  return (
    <>
      <p className="welcome__eyebrow">{PRODUCT_NAME}</p>
      <h1 ref={headingRef} tabIndex={-1} className="welcome__title">
        Your computer, <span>from your {kind}</span>
      </h1>
      <p className="welcome__lede">
        Chat with the coding agents on your computer, watch and control its screen, open its terminals and edit its files.
      </p>
      <details className="welcome__install"><summary>Get {PRODUCT_NAME} for your computer</summary>
        <CompanionDownload platform={platform} />
        <p className="fine">Extract the Linux or Windows archive and follow INSTALL.md. Then choose Pair a computer for the startup and pairing steps.</p>
      </details>
      <ul className="facts">
        <li className="facts__item">
          <ShieldCheck className="facts__icon" aria-hidden />
          <div>
            <p className="facts__title">Your computer stays in charge</p>
            <p className="facts__body">It decides what this {kind} may do. You can remove the {kind} from it at any time.</p>
          </div>
        </li>
        <li className="facts__item">
          <Network className="facts__icon" aria-hidden />
          <div>
            <p className="facts__title">Direct and private</p>
            <p className="facts__body">
              The {kind} connects straight to your computer over your own private network, such as Tailscale. No account, no cloud relay.
            </p>
          </div>
        </li>
        <li className="facts__item">
          <MonitorSmartphone className="facts__icon" aria-hidden />
          <div>
            <p className="facts__title">Runs on Linux and Windows</p>
            <p className="facts__body">
              Install {PRODUCT_NAME} on your computer and stay signed in to its desktop. Agents keep running there when you
              put the {kind} away.
            </p>
          </div>
        </li>
      </ul>
      <div className="welcome__actions">
        <Button variant="primary" size="lg" block onClick={onNext}>
          Pair a computer
        </Button>
      </div>
    </>
  );
}

function Connect({
  headingRef,
  platform,
  paste,
  link,
  setPaste,
  onOffer,
}: {
  headingRef: HeadingRef;
  platform: Platform;
  paste: boolean;
  link?: OpenedLink;
  setPaste: (v: boolean) => void;
  onOffer: (o: PairingOffer, alerts: boolean | undefined, linked: boolean) => void;
}) {
  const [scanning, setScanning] = useState(false);
  const [text, setText] = useState("");
  const [rejected, setRejected] = useState<{ problem: PairingLinkProblem | "expired"; scanned: boolean } | null>(null);
  const canScan = Boolean(platform.scanPairingCode);
  const notice = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLInputElement>(null);
  // Choosing to paste moves focus to the field, so the link can go straight
  // in, and brings all of it, hint included, into view. The on-screen
  // keyboard then shrinks the page and the browser keeps only the input in
  // view, so each resize brings the whole field back while it has focus.
  useEffect(() => {
    const input = field.current;
    const page = input?.closest(".welcome");
    if (!input || !page) return;
    const reveal = () => input.parentElement?.scrollIntoView?.({ block: "nearest" });
    if (paste) {
      input.focus({ preventScroll: true });
      reveal();
    }
    if (typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      // A frame later, once the pinned actions have been measured at the new size.
      frame = requestAnimationFrame(() => {
        if (document.activeElement === input) reveal();
      });
    });
    observer.observe(page);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [paste]);
  // The reason appears above the field or the scan button; at large text that
  // can be below the fold, so each refusal scrolls it into view.
  useEffect(() => {
    if (rejected) notice.current?.scrollIntoView?.({ block: "start" });
  }, [rejected]);

  const accept = (link: string, scanned: boolean, linked = false) => {
    const parsed = platform.parsePairingLink(link);
    if ("offer" in parsed && parsed.offer.expires_at > Date.now()) {
      setRejected(null);
      onOffer(parsed.offer, parsed.alerts, linked);
    } else {
      setRejected({ problem: "problem" in parsed ? parsed.problem : "expired", scanned });
    }
  };
  const opened = useRef<OpenedLink | undefined>(undefined);
  useEffect(() => {
    if (!link || opened.current === link) return;
    opened.current = link;
    accept(link.url, false, true);
  });

  const scan = async () => {
    setScanning(true);
    setRejected(null);
    const link = await platform.scanPairingCode?.();
    setScanning(false);
    if (link) accept(link, true);
  };

  return (
    <>
      <h1 ref={headingRef} tabIndex={-1} className="welcome__title">
        Pair with your computer
      </h1>
      <CompanionDownload platform={platform} />
      <ol className="steps">
        <li className="steps__item">
          <span className="steps__num" aria-hidden>1</span>
          <div className="steps__text"><p>Install {PRODUCT_NAME} on your computer</p>
            <p className="fine">Extract the Linux or Windows archive and open <InlineCode>INSTALL.md</InlineCode> inside the archive. Keep both executables together.</p>
            <details className="welcome__install">
              <summary>Set up {PRODUCT_NAME}</summary>
              <p className="fine">Linux: the installer starts setup when it finishes. Otherwise run it as your ordinary user, in a terminal in your graphical login session.</p>
              <code className="cmd">{`"$HOME/.local/bin/${PROGRAM_NAME}" setup`}</code>
              <p className="fine">Windows: in PowerShell in the extracted folder, not as administrator.</p>
              <code className="cmd">.\{PROGRAM_NAME}.exe setup</code>
              <p className="fine">Setup starts {PRODUCT_NAME} now and whenever you sign in, and its phone step shows the code to scan here.</p>
            </details>
          </div>
        </li>
        <li className="steps__item">
          <span className="steps__num" aria-hidden>
            2
          </span>
          <div className="steps__text">
            <p>On your computer, go on to setup's phone step, or open a terminal and run</p>
            <code className="cmd">{PROGRAM_NAME} pair</code>
          </div>
        </li>
        <li className="steps__item">
          <span className="steps__num" aria-hidden>
            3
          </span>
          <div className="steps__text">
            <p>It shows a QR code for a few minutes. Scan it with this {deviceKind()}, then confirm on the computer when it asks.</p>
          </div>
        </li>
      </ol>

      {rejected && <div ref={notice} className="welcome__notice"><Rejected {...rejected} canScan={canScan} /></div>}

      {paste || !canScan ? (
        <form
          className="welcome__paste"
          onSubmit={(e) => {
            e.preventDefault();
            accept(text, false);
          }}
        >
          {/* No url input mode: on Android it replaces the no-suggestions flag, so the keyboard would compose, correct and learn the link. */}
          <TextField
            ref={field}
            label="Pairing link"
            value={text}
            onChange={setText}
            hint={`Above the QR code on the computer. It starts with ${PAIRING_SCHEME}://pair`}
            autoComplete="off"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            className="mono-input"
          />
          <div className="welcome__actions">
            <Button type="submit" variant="primary" size="lg" block disabled={text.trim() === ""}>
              Continue
            </Button>
            {canScan && (
              <Button variant="quiet" block icon={Camera} onClick={() => setPaste(false)}>
                Scan the code instead
              </Button>
            )}
          </div>
        </form>
      ) : (
        <div className="welcome__actions">
          <Button variant="primary" size="lg" block icon={Camera} pending={scanning} onClick={() => void scan()}>
            {scanning ? "Opening camera" : "Scan the code"}
          </Button>
          <Button variant="quiet" block icon={ClipboardPaste} onClick={() => setPaste(true)}>
            Paste the link instead
          </Button>
        </div>
      )}

      <p className="fine welcome__fine">
        The code works once and expires soon. It pairs only this {deviceKind()} and can’t be reused as a login link.
      </p>
    </>
  );
}

/** Say why a link was refused and how to get one that works. */
function Rejected({ problem, scanned, canScan }: { problem: PairingLinkProblem | "expired"; scanned: boolean; canScan: boolean }) {
  const what = scanned ? "pairing code" : "pairing link";
  const pair = <InlineCode className="cmd cmd--inline">{PROGRAM_NAME} pair</InlineCode>;
  const title =
    problem === "expired" ? `That ${what} has expired` : problem === "incomplete" ? `That ${what} is incomplete` : `That isn’t a ${what}`;
  return (
    <Notice tone="stop" title={title}>
      {problem === "expired" ? (
        <>Run {pair} on the computer again, then {scanned ? "scan the new code" : canScan ? "copy the new link or scan the new code" : "copy the new link"}.</>
      ) : scanned ? (
        <>Scan the code that {pair} shows on the computer.</>
      ) : (
        <>Copy it again from {pair} on the computer{canScan ? ", or scan the code" : ""}.</>
      )}
    </Notice>
  );
}

/** Used until the platform reports the device's own name, and when it has none. */
function defaultDeviceName(): string {
  const kind = deviceKind();
  return /Android/i.test(navigator.userAgent) ? `Android ${kind}` : kind === "tablet" ? "Tablet" : "Phone";
}

const DEVICE_NAME_MAX = 64;

/** The name this device goes by in its own settings, falling back to its kind. */
function useDeviceName(platform: Platform): string {
  const [name, setName] = useState(defaultDeviceName);
  useEffect(() => {
    let active = true;
    void platform.appInfo?.().then(info => {
      const own = info.deviceName?.trim().slice(0, DEVICE_NAME_MAX);
      if (active && own) setName(own);
    }, () => {});
    return () => { active = false; };
  }, [platform]);
  return name;
}

/** How an attempt ended without pairing, when that has a screen of its own. */
type Ended = "refused" | "expired";

/** Most computers this device remembers; the native store holds the same limit. */
const MAX_COMPUTERS = 32;

function Confirm({
  headingRef,
  platform,
  offer,
  linked,
  onRescan,
  onDone,
}: {
  headingRef: HeadingRef;
  platform: Platform;
  offer: PairingOffer;
  linked?: boolean;
  onRescan: () => void;
  onDone: (r: PairingResult) => void;
}) {
  const left = useExpiry(offer.expires_at);
  const suggested = useDeviceName(platform);
  // Stays null until the person types, so a name the platform reports late still fills the field.
  const [edited, setEdited] = useState<string | null>(null);
  const name = edited ?? suggested;
  const [pending, setPending] = useState(false);
  const [code, setCode] = useState<string | null>(null);
  const [ended, setEnded] = useState<Ended | null>(null);
  const [error, setError] = useState<string | null>(null);
  const expired = left <= 0;
  const [paired, setPaired] = useState<HostProfile[] | null>(null);
  useEffect(() => {
    let active = true;
    void platform.profiles().then(profiles => {
      if (active) setPaired(profiles);
    }, () => {
      if (active) setError("Couldn’t check the computers already paired with this device. Use a new code to try again.");
    });
    return () => { active = false; };
  }, [platform]);
  // One pairing per computer: pairing again replaces it, once the person says so.
  const existing = paired?.find(p => p.hostId === offer.host_id);
  const [replace, setReplace] = useState(false);
  const full = !existing && (paired?.length ?? 0) >= MAX_COMPUTERS;

  // Attempts are numbered so a withdrawn one never reports back. Leaving this
  // screen while the computer waits for its owner withdraws the request.
  const attempt = useRef(0);
  const waiting = useRef(false);
  useEffect(() => () => {
    if (waiting.current) void platform.cancelPairing();
  }, [platform]);
  const view = ended ?? (code ? "waiting" : "form");
  useEffect(() => {
    if (view !== "form") headingRef.current?.focus();
  }, [view, headingRef]);

  const pair = async () => {
    const id = ++attempt.current;
    waiting.current = true;
    setPending(true);
    setError(null);
    try {
      const result = await platform.pair(offer, name.trim() || suggested, Boolean(existing) && replace, (shown) => {
        if (attempt.current === id) setCode(shown);
      });
      if (attempt.current !== id) return;
      waiting.current = false;
      onDone(result);
    } catch (e) {
      if (attempt.current !== id) return;
      waiting.current = false;
      setPending(false);
      setCode(null);
      const outcome = endedBy(e);
      if (outcome) setEnded(outcome);
      else setError(pairError(e, offer));
    }
  };
  const cancel = () => {
    attempt.current++;
    waiting.current = false;
    setPending(false);
    setCode(null);
    void platform.cancelPairing();
  };

  const command = <InlineCode className="cmd cmd--inline">{PROGRAM_NAME} pair</InlineCode>;
  if (ended) {
    return (
      <>
        <h1 ref={headingRef} tabIndex={-1} className="welcome__title">
          {ended === "refused" ? `Not paired with ${offer.host_name}` : "The code expired"}
        </h1>
        <p className="welcome__lede">
          {ended === "refused"
            ? "The request was refused on the computer. Nothing was paired, and that code no longer works."
            : `Nobody confirmed this ${deviceKind()} on ${offer.host_name} before the code expired, so nothing was paired.`}
        </p>
        <p className="welcome__body">
          To try again, run {command} on the computer, scan the new code, then confirm on the computer when it asks.
        </p>
        <div className="welcome__actions">
          <Button variant="primary" size="lg" block icon={Camera} onClick={onRescan}>
            Use a new code
          </Button>
        </div>
      </>
    );
  }
  if (code) {
    return (
      <>
        <h1 ref={headingRef} tabIndex={-1} className="welcome__title">
          Check the code on {offer.host_name}
        </h1>
        <div className="offer">
          <div className="offer__fp">
            <p className="offer__label">Code</p>
            <p className="pair-code mono" aria-label={`Code ${code.split("").join(" ")}`}>
              {codeWords(code)}
            </p>
          </div>
          <p className={`offer__expiry${expired ? " is-expired" : left < 60_000 ? " is-soon" : ""}`} aria-live="off">
            {expired ? "Code expired" : `Code expires in ${countdown(left)}`}
          </p>
        </div>
        <p className="welcome__body">
          The terminal where you ran {command} asks whether to pair this {deviceKind()}. Confirm there only if it shows this code.
        </p>
        <p className="welcome__body" role="status">
          Waiting for your answer on {offer.host_name}.
        </p>
        <div className="welcome__actions">
          <Button variant="secondary" size="lg" block onClick={cancel}>
            Cancel
          </Button>
        </div>
      </>
    );
  }

  return (
    <>
      <h1 ref={headingRef} tabIndex={-1} className="welcome__title">
        Pair with {offer.host_name}?
      </h1>
      {linked && <Notice title="A link opened this pairing">
        Pair only with a computer you’re setting up now, and check the fingerprint against its screen.
      </Notice>}
      {existing && <Notice title={`Already paired with ${existing.name} since ${new Date(existing.pairedAt).toLocaleDateString()}`}>
        This {deviceKind()} keeps one pairing for each computer, so pairing again replaces that one here. When you confirm on
        the computer, it removes the earlier device and gives this pairing that device’s permissions. Your drafts and Inbox items move to the new pairing.
      </Notice>}
      {full && <Notice tone="stop" title={`This ${deviceKind()} already remembers ${MAX_COMPUTERS} computers`}>
        Forget one of them in Connections, then scan a new code to pair with {offer.host_name}.
      </Notice>}
      {paired === null && !error && <p role="status">Checking existing pairings…</p>}

      <div className="offer">
        <div className="offer__head">
          <span className="offer__icon" aria-hidden>
            <Laptop />
          </span>
          <div className="offer__who">
            <p className="offer__name">{offer.host_name}</p>
            <p className="offer__addr mono">{offer.addresses.join(", ")}</p>
          </div>
        </div>
        <div className="offer__fp">
          <p className="offer__label">Fingerprint</p>
          <p className="offer__fpvalue mono" aria-label={`Fingerprint ${fingerprintWords(offer.cert_sha256).split("").join(" ")}`}>
            {fingerprintPairs(offer.cert_sha256).map((pair, i) => <span key={i}>{pair}</span>)}
          </p>
          <p className="fine">Check it matches the fingerprint shown under the code on your computer.</p>
        </div>
        <p className={`offer__expiry${expired ? " is-expired" : left < 60_000 ? " is-soon" : ""}`} aria-live="off">
          {expired ? "Code expired" : `Code expires in ${countdown(left)}`}
        </p>
      </div>

      {expired ? (
        <Notice tone="warn" title="This code has expired">
          Run {command} on the computer again and scan the new code.
        </Notice>
      ) : !full && (
        <>
          <TextField
            label={`Name this ${deviceKind()}`}
            value={name}
            onChange={setEdited}
            hint="Shown in the computer’s list of paired devices."
            maxLength={DEVICE_NAME_MAX}
            autoComplete="off"
          />
          {existing && (
            <label className="welcome__check">
              <input type="checkbox" checked={replace} disabled={pending} onChange={e => setReplace(e.target.checked)} />
              Replace the earlier pairing on this {deviceKind()}
            </label>
          )}
          <p className="welcome__body">
            {offer.host_name} decides what this {deviceKind()} may do. You’ll see exactly what it allowed on the next screen.
          </p>
        </>
      )}

      {error && (
        <Notice tone="stop" title="Not paired" className="welcome__notice">
          {error}
        </Notice>
      )}

      <div className="welcome__actions">
        {!expired && !full && (
          <Button variant="primary" size="lg" block pending={pending} disabled={paired === null || (Boolean(existing) && !replace)} onClick={() => void pair()}>
            {pending ? `Pairing with ${offer.host_name}` : existing ? "Pair again" : "Pair"}
          </Button>
        )}
        {pending ? (
          // Reaching the computer can take a while on a poor network; the
          // code to compare appears once it answers.
          <Button variant="quiet" block onClick={cancel}>
            Cancel
          </Button>
        ) : (
          <Button variant={expired ? "primary" : "quiet"} size={expired ? "lg" : "md"} block icon={Camera} onClick={onRescan}>
            Use a new code
          </Button>
        )}
      </div>
    </>
  );
}

/** The fingerprint's groups two by two: a line breaks only between pairs. */
function fingerprintPairs(hex: string): string[] {
  return fingerprintWords(hex).match(/\S+ \S+/g) ?? [];
}

/** The six digits as both screens show them. */
function codeWords(code: string): string {
  return `${code.slice(0, 3)} ${code.slice(3)}`;
}

function errorCode(e: unknown): string | undefined {
  const code = typeof e === "object" && e !== null ? (e as { code?: unknown }).code : undefined;
  return typeof code === "string" ? code : undefined;
}

/** The computer's owner refused the request, or its window closed unanswered. */
function endedBy(e: unknown): Ended | null {
  switch (errorCode(e)) {
    case "permission_denied":
      return "refused";
    case "expired":
      return "expired";
    default:
      return null;
  }
}

/** Explain a failed pairing in terms of what the person can do next. */
function pairError(e: unknown, offer: PairingOffer): string {
  const raw = typeof e === "string" ? e : e instanceof Error ? e.message : typeof e === "object" && e ? JSON.stringify(e) : String(e);
  const t = raw.toLowerCase();
  const again = `Run ${PROGRAM_NAME} pair on the computer again and use the new code.`;
  if (errorCode(e) === "conflict")
    return `Another device is already asking ${offer.host_name} to pair. If it isn’t yours, refuse it on the computer. ${again}`;
  if (t.includes("incompatible") || t.includes("version"))
    return `This app and ${offer.host_name} use different versions. Update the older one, then pair again.`;
  if (t.includes("expired") || t.includes("unauthenticated") || t.includes("already")) return `The code expired or was already used. ${again}`;
  if (t.includes("certificate") || t.includes("fingerprint"))
    return `The computer that answered isn’t the one in the code. Make sure you scanned your own computer’s code, then try again.`;
  if (t.includes("timed out") || t.includes("unreachable") || t.includes("connect") || t.includes("network"))
    return `Couldn’t reach ${offer.host_name}. Check that both devices are on your private network and that ${PRODUCT_NAME} is running on the computer.`;
  if (t.includes("denied") || t.includes("forbidden")) return `${offer.host_name} refused this ${deviceKind()}. ${again}`;
  return `${raw.trim().replace(/[.!?]+$/, "")}. Nothing was changed on this ${deviceKind()}.`;
}

const GRANTS: { key: keyof Grants; label: string }[] = [
  { key: "tasks", label: "Start and steer agent tasks" },
  { key: "approvals", label: "Answer agent requests" },
  { key: "view", label: "Watch the desktop" },
  { key: "control_input", label: "Use the mouse and keyboard when you take control" },
  { key: "computer_use", label: "Let an agent use the desktop" },
  { key: "terminal", label: "Open terminals" },
  { key: "library_read", label: "Read Library folders" },
  { key: "library_write", label: "Edit Library files" },
  { key: "transfers", label: "Send and receive files" },
  { key: "admin", label: "Manage paired devices and settings" },
];

function Done({ headingRef, result, onContinue }: { headingRef: HeadingRef; result: PairingResult; onContinue: () => void }) {
  const g = result.response.grants;
  const allowed = GRANTS.filter((x) => g[x.key]);
  const denied = GRANTS.filter((x) => !g[x.key]);
  return (
    <>
      <div className="done-mark" aria-hidden>
        <Check />
      </div>
      <h1 ref={headingRef} tabIndex={-1} className="welcome__title">
        Paired with {result.profile.name}
      </h1>
      <p className="welcome__lede">{result.profile.name} allows this {deviceKind()} to:</p>
      <ul className="grants">
        {allowed.map((x) => (
          <li key={x.key} className="grants__item">
            <Check className="grants__icon grants__icon--yes" aria-hidden />
            {x.label}
          </li>
        ))}
        {denied.map((x) => (
          <li key={x.key} className="grants__item grants__item--no">
            <Minus className="grants__icon" aria-hidden />
            <span>
              <span className="visually-hidden">Not allowed: </span>
              {x.label}
            </span>
          </li>
        ))}
      </ul>
      <p className="fine welcome__fine">
        Remove this {deviceKind()} at any time from Connections in this app, or on the computer with{" "}
        <InlineCode className="cmd cmd--inline">{PROGRAM_NAME} devices revoke</InlineCode>.
        {denied.length > 0 && (
          <>
            {" "}
            To allow more, run <InlineCode className="cmd cmd--inline">{PROGRAM_NAME} devices allow</InlineCode> on the computer.
          </>
        )}
      </p>
      <div className="welcome__actions">
        <Button variant="primary" size="lg" block onClick={onContinue}>
          Continue
        </Button>
      </div>
    </>
  );
}
