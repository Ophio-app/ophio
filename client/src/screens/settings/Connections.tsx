// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { useEffect, useMemo, useRef, useState } from "react";
import { CommandError } from "../../host/session";
import { commandResult, reconnected, selfRevoked } from "../../host/commandResult";
import { useWarning } from "../../app/warnings";
import { RefreshCw, ShieldCheck } from "lucide-react";
import {
  describeError,
  useApp,
  useHost,
  useLink,
  shallowEqual,
} from "../../app/context";
import { useNow } from "../../design/time";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { CHECK_WORD } from "../../design/remedy";
import { Remedy } from "../../design/Remedy";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { useTightLayout } from "../../shell/keyboard";
import { useStore } from "../../state/store";
import type {
  Availability,
  ChannelIdentity,
  ChannelKind,
  Command,
  CommandReceipt,
  DeviceRecord,
  Grants,
  PendingChannelLink,
  ReadinessArea,
} from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { StatusSheet, fingerprint } from "../../shell/StatusSheet";
import type { RouteProps } from "../types";
import { CHANNEL_NAME, GRANT_WORDS, grantWords, withGrant } from "./helpers";
import { PROGRAM_NAME } from "../../brand";
import { NotAllowed } from "../../shell/NotAllowed";
import { notSeenSince } from "./deviceActivity";
import { PairedComputers, sharedNames } from "./PairedComputers";
import { showPreviews } from "../control/parts";
import "./settings.css";
import { InlineCode } from "../../design/InlineCode";
type Action =
  | { kind: "rename"; device: DeviceRecord }
  | { kind: "revoke"; device: DeviceRecord }
  | { kind: "permissions"; device: DeviceRecord; grants: Grants }
  | { kind: "identity"; identity: ChannelIdentity }
  | { kind: "chat_project"; identity: ChannelIdentity; project: string }
  | { kind: "forget" }
  /** A code is checked first; `review` holds who asked for it and from where. */
  | { kind: "link"; review?: { code: string; link: PendingChannelLink } };
interface Form {
  name: string;
  code: string;
}
const SETTINGS_COMMANDS = new Set([
  "rename_device",
  "revoke_device",
  "set_device_grants",
  "approve_channel_link",
  "refuse_channel_link",
  "revoke_channel_identity",
  "set_channel_project",
  "run_readiness_check",
]);
/** What each readiness area covers, in the words the rest of the app uses. */
const AREA_WORDS: Record<ReadinessArea, string> = {
  network: "Connection",
  terminal: "Terminal",
  graphics: "Screen and input",
  accounts: "Agents and accounts",
  storage: "Library and transfers",
  speech: "Voice",
  channels: "Chat links",
};
const LINK_ACCESS = "The computer decides this identity’s permissions. Messages in that chat are also held by its chat service.";
const LINK_REVIEW = "In a group chat, anyone can ask the bot for a code, and a name can be copied. The user id can’t.";
export default function Connections(_props: RouteProps<"connections">) {
  const app = useApp(),
    { profile, session } = app,
    link = useLink(),
    tight = useTightLayout();
  const host = useHost((s) => s.host),
    reported = useHost((s) => s.devices),
    identities = useHost((s) => s.identities),
    projects = useHost((s) => s.projects),
    readiness = useHost((s) => s.readiness),
    capabilities = useHost((s) => s.capabilities),
    grants = useHost((s) => s.grants),
    lastHeard = useHost((s) => s.lastHeardAt);
  const devices = useMemo(
    () => standing(reported, profile.deviceId),
    [reported, profile.deviceId],
  );
  const commands = useStore(
    session.commands,
    (s) =>
      Object.values(s).filter((c) => SETTINGS_COMMANDS.has(c.body.command)),
    shallowEqual,
  );
  const now = useNow();
  const unconfirmed = (c: (typeof commands)[number]) =>
    c.phase === "sending" || c.phase === "unknown" || c.error?.code === "outcome_unknown";
  const latestSettled = commands.filter(c => !unconfirmed(c) && now - c.issuedAt < 60_000).at(-1);
  const recentCommands = commands.filter(c => unconfirmed(c) || c === latestSettled);
  const refusal = useRef<HTMLDivElement>(null);
  const [action, setAction] = useState<Action | null>(null),
    [status, setStatus] = useState(false),
    [pending, setPending] = useState(false),
    [loading, setLoading] = useState(false),
    /** Only a failed load leaves the lists unknown; a refused change doesn't. */
    [loadFailed, setLoadFailed] = useState(false),
    [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (error) refusal.current?.scrollIntoView?.({ block: "nearest" });
  }, [error]);
  const [form, setForm] = useState<Form>(() => {
    try {
      return {
        name: "",
        code: "",
        ...(JSON.parse(
          localStorage.getItem(`connections-form.${profile.profileId}`) ?? "{}",
        ) as Partial<Form>),
      };
    } catch {
      return { name: "", code: "" };
    }
  });
  const patch = (value: Partial<Form>) => {
    const next = { ...form, ...value };
    setForm(next);
    try {
      localStorage.setItem(
        `connections-form.${profile.profileId}`,
        JSON.stringify({ ...next, code: "" }),
      );
    } catch {
      setError(
        "This form isn’t saved on this device. Keep this screen open until you finish.",
      );
    }
  };
  const uncertain = commands.some((c) => c.phase === "unknown");
  const online = link.state === "online",
    admin = online && !!grants?.admin && !uncertain,
    /** Paired devices, chat links and readiness are shown only to a device
     * that may manage access; the computer refuses the others. */
    hidden = online && !!grants && !grants.admin,
    /** A computer that can't show who asked for a code gets no blind approval. */
    reviewsLinks = !!capabilities?.features.includes("channel_link_review");
  const emptyReport = (empty: string) => !online ? "Not loaded while offline" : loading
    ? "Loading from the computer…" : loadFailed ? "Not loaded. Reconnect to check." : empty;
  useEffect(() => {
    if (!online) return;
    let cancelled = false;
    setLoading(true);
    setLoadFailed(false);
    setError(null);
    const before = session.state.get();
    const jobs = [
      hidden ? null : session.query({ kind: "devices" }).then((r) => {
        if (!cancelled)
          session.state.set((s) => ({
            ...s,
            devices: s.devices === before.devices ? r.devices : s.devices,
            identities:
              s.identities === before.identities ? r.identities : s.identities,
          }));
      }),
      hidden ? null : session.query({ kind: "readiness" }).then((r) => {
        if (!cancelled)
          session.state.set((s) => ({
            ...s,
            readiness:
              s.readiness === before.readiness ? r.report : s.readiness,
          }));
      }),
    ];
    void Promise.allSettled(jobs).then((results) => {
      if (cancelled) return;
      setLoading(false);
      const failed = results.find((r) => r.status === "rejected");
      if (failed?.status === "rejected") {
        setLoadFailed(true);
        setError(describeError(failed.reason, profile.name));
      }
    });
    return () => {
      cancelled = true;
    };
  }, [online, hidden, session, profile.name]);
  const currentDevice = devices.find((d) => d.is_current && !d.revoked_at);
  /** The command's effect. Revoking this device ends its own connection, so
   * the computer's refusal to take it back is the confirmation. Changing its
   * own grants also ends the connection; the result is read after it
   * reconnects under them. */
  const settled = async (body: Command, receipt: CommandReceipt) => {
    try {
      await commandResult(session, receipt);
    } catch (e) {
      const own =
        "device_id" in body && body.device_id === currentDevice?.device_id;
      if (own && body.command === "revoke_device" && (await selfRevoked(session)))
        return;
      if (own && body.command === "set_device_grants" && (await reconnected(session))) {
        await commandResult(session, receipt);
        return;
      }
      throw e;
    }
  };
  const execute = async (body: Command, label: string, close = true) => {
    setPending(true);
    setError(null);
    try {
      const receipt = await session.command(body, { label });
      await settled(body, receipt);
      // A decided code is spent, so the next "Link a chat" starts empty.
      if (body.command === "approve_channel_link" || body.command === "refuse_channel_link") patch({ code: "" });
      if (close) closeAction();
      return receipt;
    } catch (e) {
      setError(((body.command === "approve_channel_link" || body.command === "refuse_channel_link") && linkRefusal(e)) || describeError(e, profile.name));
      if (e instanceof CommandError && e.error.code === "device_grants_changed" && body.command === "set_device_grants") {
        try {
          const { devices } = await session.query({ kind: "devices" });
          session.state.set((s) => ({ ...s, devices }));
          const device = devices.find((d) => d.device_id === body.device_id);
          if (device) setAction({ kind: "permissions", device, grants: { ...device.grants } });
        } catch (refreshError) {
          setError(`${describeError(e, profile.name)} ${describeError(refreshError, profile.name)}`);
        }
      }
      return null;
    } finally {
      setPending(false);
    }
  };
  const review = async (code: string) => {
    setPending(true);
    setError(null);
    try {
      const { link } = await session.query({ kind: "channel_link", link_code: code });
      setAction({ kind: "link", review: { code, link } });
    } catch (e) {
      setError(linkRefusal(e) || describeError(e, profile.name));
    } finally {
      setPending(false);
    }
  };
  const open = (a: Action) => {
    setError(null);
    if (a.kind === "rename") patch({ name: a.device.name });
    setAction(a);
  };
  const revokeWarning = useWarning("device.revoke");
  const chatWarning = useWarning("chat.revoke");
  const revoke = (device: DeviceRecord) => void execute({ command: "revoke_device", device_id: device.device_id }, `Revoke ${device.name}`);
  const revokeChat = (identity: ChannelIdentity) => void execute({ command: "revoke_channel_identity", identity_id: identity.id }, `Revoke ${identity.display_name}`);
  const closeAction = () => setAction(null);
  const forget = async (revoke: boolean) => {
    setPending(true);
    setError(null);
    try {
      if (revoke && currentDevice) {
        const body: Command = {
          command: "revoke_device",
          device_id: currentDevice.device_id,
        };
        await settled(
          body,
          await session.command(body, { label: "Revoke this device" }),
        );
      }
      await app.forget();
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setPending(false);
    }
  };
  const areas = [...new Set(readiness?.checks.map((c) => c.area) ?? [])],
    linkReview = action?.kind === "link" ? action.review : undefined;
  return (
    <Screen
      title="Connections"
      subtitle={profile.name}
      className="settings-screen"
    >
      <div className="settings-stack">
        <PairedComputers />
        {loading && (
          <p role="status" className="settings-meta">
            Loading access and readiness…
          </p>
        )}
        {!online && (
          <Notice
            tone="unknown"
            title={
              link.state === "failed" && link.reason === "revoked"
                ? "This device’s access was revoked"
                : "Computer disconnected"
            }
          >
            Showing the last known state
            {lastHeard ? (
              <>
                {" "}
                · <Age at={lastHeard} suffix=" ago" />
              </>
            ) : (
              ". No observations yet."
            )}
            . Changes to access need a connection.
          </Notice>
        )}
        {hidden && (
          <NotAllowed
            what="manage access"
            permission="manage"
            detail={`${profile.name} doesn’t show it paired devices, chat links or readiness.`}
          />
        )}
        {error && (
          <Notice tone={uncertain ? "unknown" : "stop"}>
            {uncertain
              ? "The computer hasn’t confirmed the result. Reconnect to check before trying again."
              : error}
          </Notice>
        )}
        {uncertain && (
          <Notice tone="unknown">
            The computer hasn’t confirmed a previous change. Access changes are
            paused until its receipt is recovered.
          </Notice>
        )}
        {recentCommands.length > 0 && (
          <div
            className="settings-command-list"
            aria-label="Recent settings requests"
          >
            {recentCommands.map((c) => (
              <div key={c.id}>
                <strong>{c.label}</strong>
                <StateLine
                  phase={
                    c.phase === "sending"
                      ? "sending"
                      : c.phase === "unknown"
                        ? "unknown"
                        : c.phase === "rejected"
                          ? "rejected"
                          : c.phase === "not_delivered"
                            ? "not_delivered"
                            : "host_accepted"
                  }
                  computer={profile.name}
                  detail={c.error?.message}
                />
              </div>
            ))}
          </div>
        )}
        <Section title="This computer" id="settings-computer">
          <div className="settings-card settings-stack">
            <div className="settings-computer">
              <ShieldCheck aria-hidden />
              <div>
                <h2>{profile.name}</h2>
                <p className="settings-meta">
                  {host?.os_label ?? "Operating system unknown"}
                </p>
              </div>
            </div>
            <dl className="settings-facts">
              {host && host.name !== profile.name && (
                <div>
                  <dt>Name on the computer</dt>
                  <dd>{host.name}</dd>
                </div>
              )}
              {sharedNames(app.profiles).has(profile.profileId) && (
                <div>
                  <dt>{profile.addresses.length === 1 ? "Address" : "Addresses"}</dt>
                  <dd className="mono">{profile.addresses.join(", ")}</dd>
                </div>
              )}
              <div>
                <dt>{PRODUCT_NAME} version</dt>
                <dd>{host?.companion_version ?? "Unknown"}</dd>
              </div>
              <div>
                <dt>Paired</dt>
                <dd>{new Date(profile.pairedAt).toLocaleDateString()}</dd>
              </div>
              <div>
                <dt>Pinned certificate</dt>
                <dd className="mono">{fingerprint(profile.certSha256)}</dd>
              </div>
            </dl>
            <Button variant="quiet" onClick={() => open({ kind: "forget" })}>
              Forget this computer on this device
            </Button>
          </div>
        </Section>
        <Section title="Devices with access" id="settings-phones">
          <div className="settings-stack">
            {hidden ? (
              <p className="settings-meta">Hidden from this device.</p>
            ) : devices.map((device) => (
              <article
                className="settings-card settings-stack"
                key={device.device_id}
              >
                <div>
                  <h3>
                    {device.name}
                    {device.is_current ? " · this device" : ""}
                  </h3>
                  <p className="settings-meta">
                    {device.platform} · Paired{" "}
                    {new Date(device.paired_at).toLocaleDateString()}
                  </p>
                </div>
                {device.revoked_at ? (
                  <p className="settings-revoked">
                    Access revoked ·{" "}
                    <Age at={device.revoked_at} suffix=" ago" />
                  </p>
                ) : (
                  <>
                    <p className="settings-meta">
                      {device.is_current && online ? "Connected" : "Last seen "}
                      {device.is_current && online ? null : device.last_seen_at ? (
                        <Age at={device.last_seen_at} suffix=" ago" />
                      ) : (
                        "unknown"
                      )}
                    </p>
                    <p className="settings-meta">{grantWords(device.grants)}</p>
                    {notSeenSince(device, devices, now) !== null && <Notice tone="warn"
                      title={`Not seen since ${new Date(notSeenSince(device, devices, now) ?? 0).toLocaleDateString()}`}>
                      Another device has paired since. This record still has access; revoke it if it is no longer used.
                    </Notice>}
                    <div className="settings-actions">
                      <Button
                        disabled={!admin || pending}
                        onClick={() => open({ kind: "rename", device })}
                      >
                        Rename {device.name}
                      </Button>
                      <Button
                        disabled={!admin || pending}
                        onClick={() =>
                          open({ kind: "permissions", device, grants: device.grants })
                        }
                      >
                        Permissions for {device.name}
                      </Button>
                      <Button
                        disabled={!admin || pending}
                        variant="stop-outline"
                        // Revoking this device ends its own access, so that
                        // one always asks.
                        onClick={() => device.is_current ? open({ kind: "revoke", device })
                          : revokeWarning.ask(() => open({ kind: "revoke", device }), () => revoke(device))}
                      >
                        Revoke {device.name}
                      </Button>
                    </div>
                  </>
                )}
              </article>
            ))}
            {!hidden && !devices.length && !loading && (
              <p className="settings-meta">{emptyReport("No devices have been reported.")}</p>
            )}
          </div>
        </Section>
        <Section title="Chat" id="settings-chat">
          <div className="settings-stack">
            {hidden ? (
              <p className="settings-meta">Hidden from this device.</p>
            ) : identities.map((identity) => (
              <article
                className="settings-card settings-stack"
                key={identity.id}
              >
                <h3>{identity.display_name}</h3>
                <p className="settings-meta">{CHANNEL_NAME[identity.channel]} chat</p>
                <details className="settings-meta">
                  <summary>{CHANNEL_NAME[identity.channel]} ids<span className="visually-hidden"> for {identity.display_name}</span></summary>
                  <p>{chatIds(identity)}</p>
                </details>
                <p className="settings-meta">
                  Linked <Age at={identity.linked_at} suffix=" ago" />
                </p>
                {identity.revoked_at ? (
                  <p className="settings-revoked">
                    Access revoked ·{" "}
                    <Age at={identity.revoked_at} suffix=" ago" />
                  </p>
                ) : (
                  <>
                    <ConnectorSetup
                      channel={identity.channel}
                      connector={capabilities?.[identity.channel]}
                    />
                    <p className="settings-meta">
                      {grantWords(identity.grants)}
                    </p>
                    <p className="settings-meta">
                      Project:{" "}
                      {identity.project_id
                        ? projects[identity.project_id]?.name ?? "no longer on this computer"
                        : "none"}
                    </p>
                    <div className="settings-actions">
                      <Button
                        disabled={!admin || pending}
                        aria-label={`${identity.project_id ? "Change" : "Choose"} project for ${identity.display_name}`}
                        onClick={() =>
                          open({
                            kind: "chat_project",
                            identity,
                            project:
                              identity.project_id && projects[identity.project_id]
                                ? identity.project_id
                                : "",
                          })
                        }
                      >
                        {identity.project_id ? "Change project" : "Choose project"}
                      </Button>
                      <Button
                        disabled={!admin || pending}
                        variant="stop-outline"
                        onClick={() => chatWarning.ask(() => open({ kind: "identity", identity }), () => revokeChat(identity))}
                      >
                        Revoke {identity.display_name}
                      </Button>
                    </div>
                  </>
                )}
              </article>
            ))}
            {!hidden && !identities.length && (
              <p className="settings-meta">{emptyReport("No chats linked.")}</p>
            )}
            <Button
              disabled={!admin || pending}
              onClick={() => open({ kind: "link" })}
            >
              Link a chat
            </Button>
          </div>
        </Section>
        <Section title="Previews" id="settings-previews">
          <div className="settings-card settings-stack">
            <p className="settings-meta">
              Previews are added, opened and removed in Control, next to
              Screens and Terminals.
            </p>
            <Button onClick={() => showPreviews(app.ui)}>
              Open Previews in Control
            </Button>
          </div>
        </Section>
        <Section title="Readiness" id="settings-readiness">
          <div className="settings-card settings-stack">
            {hidden ? (
              <p className="settings-meta">Hidden from this device.</p>
            ) : readiness ? (
              <>
                <p className="settings-meta">
                  Checked <Age at={readiness.checked_at} suffix=" ago" />
                </p>
                {areas.map((area) => (
                  <div key={area}>
                    <h3 className="settings-area">{AREA_WORDS[area] ?? area}</h3>
                    <ul className="settings-capabilities">
                      {readiness.checks
                        .filter((c) => c.area === area)
                        .map((c) => (
                          <li key={c.label}>
                            <strong>
                              {c.summary ?? `${c.label} · ${CHECK_WORD[c.status]}`}
                            </strong>
                            {c.detail && <span>{c.detail}</span>}
                            {c.remedy && <span><Remedy text={c.remedy} /></span>}
                          </li>
                        ))}
                    </ul>
                  </div>
                ))}
              </>
            ) : (
              <p>{emptyReport("No readiness report yet.")}</p>
            )}
            <div className="settings-actions">
              <Button
                icon={RefreshCw}
                pending={pending}
                disabled={!online || hidden}
                onClick={() =>
                  void execute(
                    { command: "run_readiness_check" },
                    "Check readiness",
                    false,
                  )
                }
              >
                Check again
              </Button>
              <Button onClick={() => setStatus(true)}>
                Open connection details
              </Button>
            </div>
          </div>
        </Section>
      </div>
      <StatusSheet open={status} onClose={() => setStatus(false)} />
      <Sheet
        open={!!action}
        onClose={() => {
          if (!pending) closeAction();
        }}
        title={actionTitle(action)}
        footer={
          action && (
            <>
              {action.kind === "rename" && (
                <Button
                  variant="primary"
                  pending={pending}
                  disabled={!admin || !form.name.trim()}
                  onClick={() =>
                    void execute(
                      {
                        command: "rename_device",
                        device_id: action.device.device_id,
                        name: form.name.trim(),
                      },
                      `Rename ${action.device.name}`,
                    )
                  }
                >
                  Save name
                </Button>
              )}
              {action.kind === "permissions" && (
                <Button
                  variant="primary"
                  pending={pending}
                  disabled={!admin || !changedGrants(action)}
                  onClick={() =>
                    void execute(
                      {
                        command: "set_device_grants",
                        device_id: action.device.device_id,
                        expected: action.device.grants,
                        grants: action.grants,
                      },
                      `Change permissions for ${action.device.name}`,
                    )
                  }
                >
                  Save permissions
                </Button>
              )}
              {action.kind === "revoke" && (
                <Button
                  variant="stop-outline"
                  size="lg"
                  pending={pending}
                  disabled={!admin}
                  onClick={() => { if (!action.device.is_current) revokeWarning.agreed(); revoke(action.device); }}
                >
                  Revoke {action.device.name}
                </Button>
              )}
              {action.kind === "identity" && (
                <Button
                  variant="stop-outline"
                  pending={pending}
                  disabled={!admin}
                  onClick={() => { chatWarning.agreed(); revokeChat(action.identity); }}
                >
                  Revoke {action.identity.display_name}
                </Button>
              )}
              {action.kind === "chat_project" && (
                <>
                  <Button
                    variant="primary"
                    pending={pending}
                    disabled={
                      !admin ||
                      !projects[action.project] ||
                      action.project === action.identity.project_id
                    }
                    onClick={() =>
                      void execute(
                        {
                          command: "set_channel_project",
                          identity_id: action.identity.id,
                          project_id: action.project,
                        },
                        `Choose a project for ${action.identity.display_name}`,
                      )
                    }
                  >
                    Use {projects[action.project]?.name ?? "project"}
                  </Button>
                  {action.identity.project_id && (
                    <Button
                      variant="quiet"
                      disabled={!admin || pending}
                      onClick={() =>
                        void execute(
                          {
                            command: "set_channel_project",
                            identity_id: action.identity.id,
                            project_id: null,
                          },
                          `Stop using a project for ${action.identity.display_name}`,
                        )
                      }
                    >
                      Stop using a project
                    </Button>
                  )}
                </>
              )}
              {action.kind === "link" && !action.review && (
                <Button
                  variant="primary"
                  pending={pending}
                  disabled={!admin || !reviewsLinks || !form.code.trim()}
                  // Codes are upper case; a phone keyboard types lower case.
                  onClick={() => void review(form.code.trim().toUpperCase())}
                >
                  Check code
                </Button>
              )}
              {linkReview && (
                <>
                  <Button
                    variant="primary"
                    pending={pending}
                    disabled={!admin}
                    onClick={() =>
                      void execute(
                        {
                          command: "approve_channel_link",
                          link_code: linkReview.code,
                        },
                        `Link ${linkReview.link.display_name}`,
                      )
                    }
                  >
                    Approve {linkReview.link.display_name}
                  </Button>
                  <Button
                    variant="stop-outline"
                    pending={pending}
                    disabled={!admin}
                    onClick={() =>
                      void execute(
                        {
                          command: "refuse_channel_link",
                          link_code: linkReview.code,
                        },
                        `Refuse ${linkReview.link.display_name}`,
                      )
                    }
                  >
                    Refuse this code
                  </Button>
                </>
              )}
              {action.kind === "forget" && (
                <>
                  {admin && currentDevice && (
                    <Button
                      variant="stop-outline"
                      pending={pending}
                      onClick={() => void forget(true)}
                    >
                      Revoke this device, then forget
                    </Button>
                  )}
                  <Button pending={pending} onClick={() => void forget(false)}>
                    Forget on this device only
                  </Button>
                </>
              )}
            </>
          )
        }
      >
        {action?.kind === "rename" && (
          <TextField
            label="Device name"
            value={form.name}
            onChange={(name) => patch({ name })}
            maxLength={64}
          />
        )}
        {action?.kind === "permissions" && (
          <fieldset className="settings-stack" disabled={pending}>
            <legend className="settings-meta">
              What {action.device.name} may do on {profile.name}.{" "}
              {action.device.is_current
                ? "This device reconnects to apply the change."
                : "Its open connections close and reconnect under the new permissions."}
            </legend>
            {(Object.keys(GRANT_WORDS) as (keyof Grants)[]).map((key) => (
              <label className="settings-check" key={key}>
                <input
                  type="checkbox"
                  checked={action.grants[key]}
                  // Accounts is given at the computer; a phone may only take it away.
                  disabled={key === "accounts" && !action.device.grants.accounts}
                  aria-describedby={key === "accounts" && !action.device.grants.accounts ? "accounts-grant-note" : undefined}
                  onChange={(e) =>
                    setAction({
                      ...action,
                      grants: withGrant(action.grants, key, e.target.checked),
                    })
                  }
                />
                {GRANT_WORDS[key]}
              </label>
            ))}
            {!action.device.grants.accounts && (
              <p className="settings-meta" id="accounts-grant-note">
                Only {profile.name} can allow Accounts. Run{" "}
                <InlineCode className="cmd cmd--inline">
                  {PROGRAM_NAME} devices allow {action.device.device_id} accounts
                </InlineCode>{" "}
                there.
              </p>
            )}
            {action.device.is_current && !action.grants.admin && (
              <Notice tone="warn">
                This device will no longer be able to manage access. To give it
                back, run{" "}
                <InlineCode className="cmd cmd--inline">
                  {PROGRAM_NAME} devices allow {action.device.device_id} manage
                </InlineCode>{" "}
                on {profile.name}, or use another device that manages access.
              </Notice>
            )}
          </fieldset>
        )}
        {action?.kind === "revoke" && (
          <p>
            {action.device.name} will lose access to {profile.name} immediately
            when the computer confirms revocation.{" "}
            {action.device.is_current
              ? "This ends your own access. Pair again on the computer to reconnect. Previously downloaded files stay on this device."
              : "To restore access, pair that device again on the computer. Previously downloaded files stay on that device."}
          </p>
        )}
        {action?.kind === "revoke" && !action.device.is_current && revokeWarning.check}
        {action?.kind === "identity" && (
          <p>
            {action.identity.display_name} on{" "}
            {CHANNEL_NAME[action.identity.channel]}{" "}
            will lose chat access. This doesn’t revoke paired devices or erase
            previous chat messages. Link this identity again to restore access.
          </p>
        )}
        {action?.kind === "identity" && chatWarning.check}
        {action?.kind === "chat_project" && (
          <div className="settings-stack">
            <p>
              New tasks from this chat start in this project. Everyone linked
              in the chat shares it. The chat bot reconnects to apply a change.
            </p>
            {Object.keys(projects).length ? (
              <label className="field">
                <span className="field__label">Project</span>
                <select
                  className="field__input"
                  value={action.project}
                  disabled={pending}
                  onChange={(e) => setAction({ ...action, project: e.target.value })}
                >
                  {!projects[action.project] && (
                    <option value="">Choose a project</option>
                  )}
                  {Object.values(projects).map((p) => (
                    <option key={p.project_id} value={p.project_id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
              <Notice>There are no projects on {profile.name} yet. Add one in Projects first.</Notice>
            )}
          </div>
        )}
        {action?.kind === "forget" && (
          <p>
            This removes {profile.name} from this device. The computer still
            lists this device until you revoke it there. Drafts and Inbox items stay
            on this device. Pair again on the computer to reconnect and get them back.{" "}
            {admin && currentDevice ? (
              "You can revoke this device’s access first."
            ) : (
              <>
                To revoke it, run{" "}
                <InlineCode className="cmd cmd--inline">
                  {PROGRAM_NAME} devices revoke {profile.deviceId}
                </InlineCode>{" "}
                on the computer.
              </>
            )}
          </p>
        )}
        {action?.kind === "link" && !action.review && (
          <div className="settings-stack">
            <p>
              Enter the one-time link code shown by your Telegram or Discord
              bot.{!tight && <> {LINK_ACCESS}</>}
            </p>
            {/* Short for the text size, the access note folds away so the code field and its error stay in view. */}
            {tight && <details><summary>Access and privacy</summary><p>{LINK_ACCESS}</p></details>}
            {online && !reviewsLinks && (
              <Notice tone="warn" title={`Update ${PRODUCT_NAME} on the computer`}>
                This version can’t show who asked for a code, so there’s no way
                to check a code before approving it.
              </Notice>
            )}
            <TextField
              label="Chat link code"
              value={form.code}
              onChange={(code) => patch({ code })}
              autoComplete="off"
            />
          </div>
        )}
        {linkReview && (
          <LinkReview
            link={linkReview.link}
            project={linkReview.link.project_id ? projects[linkReview.link.project_id]?.name ?? null : undefined}
            tight={tight}
          />
        )}
        {error && (
          <div ref={refusal}><Notice tone={uncertain ? "unknown" : "stop"}>
            {uncertain
              ? "The computer hasn’t confirmed the result. Reconnect to check before trying again."
              : error}
          </Notice></div>
        )}
      </Sheet>
    </Screen>
  );
}
function changedGrants(action: { device: DeviceRecord; grants: Grants }): boolean {
  return (Object.keys(GRANT_WORDS) as (keyof Grants)[]).some(
    (k) => action.grants[k] !== action.device.grants[k],
  );
}
function actionTitle(action: Action | null): string {
  if (!action) return "Connections";
  switch (action.kind) {
    case "rename":
      return `Rename ${action.device.name}`;
    case "revoke":
      return `Revoke ${action.device.name}?`;
    case "permissions":
      return `Permissions for ${action.device.name}`;
    case "identity":
      return `Revoke ${action.identity.display_name}?`;
    case "chat_project":
      return `Project for ${action.identity.display_name}`;
    case "forget":
      return "Forget this computer?";
    case "link":
      return "Link a chat";
  }
}

/** Where a chat identity lives, in the platform's own ids. */
function chatIds(c: { external_user: string; external_chat: string }): string {
  return `User ${c.external_user} · chat ${c.external_chat}`;
}

function chatAddress(c: { channel: ChannelKind; external_user: string; external_chat: string }): string {
  return `${CHANNEL_NAME[c.channel]} · user ${c.external_user} · chat ${c.external_chat}`;
}

/** Who asked for a code and from where. `project` is undefined for a chat
 * without one and null for one no longer on the computer. */
function LinkReview({ link, project, tight }: { link: PendingChannelLink; project: string | null | undefined; tight: boolean }) {
  const card = useRef<HTMLElement>(null);
  // Entering the code may have left the sheet scrolled down; the ids come first.
  useEffect(() => {
    card.current?.scrollIntoView?.({ block: "nearest" });
  }, []);
  return (
    <div className="settings-stack">
      <p>Check who asked for this code before you approve it.</p>
      <article ref={card} className="settings-card settings-stack" aria-label="Who asked for this code">
        <h3>{link.display_name}</h3>
        <p className="settings-meta">{chatAddress(link)}</p>
        <p className="settings-meta">
          Project: {project === undefined ? "none" : project ?? "no longer on this computer"}
        </p>
      </article>
      {tight ? <details><summary>Why check</summary><p>{LINK_REVIEW}</p></details> : <p>{LINK_REVIEW}</p>}
      <p className="settings-meta">
        {project
          ? `If you approve, they can start tasks and answer approvals in ${project} from this chat.`
          : "If you approve, they can start tasks and answer approvals from this chat once it has a project."}{" "}
        The bot tells them whether you approved or refused.
      </p>
    </div>
  );
}

/** A chat stays linked when its connector is switched off or loses its token
 * on the computer, but its messages stop arriving until it is set up again. */
function ConnectorSetup({ channel, connector }: { channel: ChannelKind; connector?: Availability }) {
  return connector?.state === "permission_missing" ? (
    <Notice tone="warn" title="This chat can’t reach the computer">
      <p>{CHANNEL_NAME[channel]} needs setup on the computer.</p>
      <Remedy text={connector.remedy} />
    </Notice>
  ) : null;
}

/** A code the computer no longer holds can't be approved on a retry; the chat
 * gets a new one by messaging its bot again. */
function linkRefusal(e: unknown): string | null {
  if (!(e instanceof CommandError)) return null;
  switch (e.error.code) {
    case "expired":
      return "This code has expired. Send any message to the bot to get a new code, then enter it here.";
    case "not_found":
      return "No chat is waiting for this code. Check it, or send any message to the bot to get a new code.";
    default:
      return null;
  }
}

/** This device first, then devices that still have access by when they were
 * last seen, then revoked ones. The computer's device-list updates are shared
 * by every connection, so which one is this device is known here. */
function standing(devices: DeviceRecord[], self: string): DeviceRecord[] {
  const rank = (d: DeviceRecord) =>
    d.revoked_at ? 2 : d.device_id === self ? 0 : 1;
  return devices
    .map((d) => (d.device_id === self ? { ...d, is_current: true } : d))
    .sort(
      (a, b) =>
        rank(a) - rank(b) ||
        (b.revoked_at ?? b.last_seen_at ?? 0) -
          (a.revoked_at ?? a.last_seen_at ?? 0),
    );
}
