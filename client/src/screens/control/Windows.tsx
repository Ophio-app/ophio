// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { AppWindow, Hand, RefreshCw } from "lucide-react";
import {
  describeError,
  useApp,
  useCommand,
  useHost,
  useLink,
} from "../../app/context";
import { commandResult } from "../../host/commandResult";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { CapabilityNote, Notice } from "../../design/Notice";
import { OwnershipStrip } from "../../design/Ownership";
import { useStore } from "../../state/store";
import { StateLine } from "../../design/StateLine";
import { Age } from "../../design/Age";
import { Sheet } from "../../design/Sheet";
import { announce } from "../../design/Announcer";
import { Screen } from "../../shell/Screen";
import type {
  WindowAction,
  WindowActionReceipt,
  WindowCatalog,
  WindowInfo,
} from "../../protocol/types";
import type { RouteProps } from "../types";
import { useMedia } from "./useMedia";
import { grantedLease, useControl, useVisible } from "./useControl";
import { LivePicture } from "./LivePicture";
import { SafetyActions } from "./SafetyStrip";
import { ConnectionNote } from "./Feedback";
import { ownership } from "./ownership";
import { inputBlocker } from "./readiness";
import "./windows.css";
import { useWarning } from "../../app/warnings";

const catalogs = new Map<string, WindowCatalog>();

export default function Windows({ route }: RouteProps<"windows">) {
  const { session, profile } = useApp();
  const visible = useVisible();
  const online = useLink().state === "online";
  const target = useHost((s) =>
    s.targets.find((t) => t.target_id === route.targetId),
  );
  const grants = useHost((s) => s.grants);
  const caps = useHost((s) => s.capabilities?.window_actions);
  const hostCaps = useHost((s) => s.capabilities);
  const media = useMedia(target, visible && !!grants?.view);
  const control = useControl(route.targetId, media.latest, visible);
  const run = useCommand();
  const lastCommand = useStore(session.commands, (all) =>
    Object.values(all)
      .filter(
        (c) =>
          c.body.command === "window_action" &&
          c.body.control.target_id === route.targetId,
      )
      .at(-1),
  );
  const progress = useHost((s) =>
    lastCommand ? s.commandStages[lastCommand.id] : undefined,
  );
  const recorded = useHost((s) =>
    s.interventions
      .filter(
        (r) =>
          r.targetId === route.targetId && r.receipt.kind === "window_action",
      )
      .at(-1),
  );
  const [catalog, setCatalog] = useState<WindowCatalog | null>(
    () => catalogs.get(profile.profileId) ?? null,
  );
  const [search, setSearch] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<WindowActionReceipt | null>(null);
  const [closing, setClosing] = useState<WindowInfo | null>(null);
  const warn = useWarning("window.close");
  useEffect(() => {
    if (!online || !visible) return;
    let disposed = false;
    setLoading(true);
    setError(null);
    void session
      .query({ kind: "windows", search: "" })
      .then(
        (result) => {
          if (disposed) return;
          setCatalog(result.catalog);
          catalogs.set(profile.profileId, result.catalog);
        },
        (e) => {
          if (!disposed) setError(describeError(e, profile.name));
        },
      )
      .finally(() => {
        if (!disposed) setLoading(false);
      });
    return () => {
      disposed = true;
    };
  }, [online, visible, session, profile.profileId, profile.name, refresh]);
  const acquire = async () => {
    if (!target) return;
    control.arm();
    setPending("Requesting control…");
    const reply = await run(
      {
        command: "request_human_lease",
        target_id: target.target_id,
        geometry_revision: target.geometry.revision,
      },
      { label: "Enable window actions" },
    );
    control.settled(await grantedLease(session, reply, profile.name));
    setPending(null);
  };
  const act = async (action: WindowAction, label: string) => {
    const lease = control.view?.lease;
    if (!target || !lease || !control.enabled || pending) return;
    setPending(label);
    setReceipt(null);
    setError(null);
    setClosing(null);
    try {
      const reply = await session.command(
        {
          command: "window_action",
          control: {
            target_id: target.target_id,
            lease_id: lease.lease_id,
            boot_epoch: lease.boot_epoch,
            generation: lease.generation,
            geometry_revision: target.geometry.revision,
          },
          action,
        },
        { label },
      );
      const result = await commandResult(session, reply);
      if (result?.kind === "window_action") {
        setReceipt(result.receipt);
        announce(result.receipt.detail);
        setRefresh((n) => n + 1);
      } else
        setError(
          "The computer hasn’t confirmed this window action. Check the desktop before trying again.",
        );
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setPending(null);
    }
  };
  const capability = catalog?.capabilities ?? caps;
  const needle = search.toLocaleLowerCase();
  const windows =
    catalog?.windows.filter((w) =>
      `${w.title} ${w.app ?? ""}`.toLocaleLowerCase().includes(needle),
    ) ?? [];
  const apps =
    catalog?.apps.filter((a) => a.name.toLocaleLowerCase().includes(needle)) ??
    [];
  const ready = control.enabled && online && !pending;
  const inputReason = inputBlocker(hostCaps, target, control.view ?? undefined, grants);
  const canAcquire =
    online &&
    !inputReason &&
    control.reconciled &&
    control.isFresh &&
    target?.input.state === "available" &&
    !!grants?.control_input &&
    ["ready", "paused"].includes(control.view?.input_state ?? "");
  // Why Enable control is off, so its locked actions don't go unexplained.
  const acquireWhy = !online ? "Reconnect to the computer to enable control."
    : inputReason
      ?? (control.view?.input_state === "agent" ? "The agent has control. Pause it to enable yours."
        : control.view?.input_state === "human" ? "Another device has control. It needs to give it up first."
          : control.view?.input_state === "revoking" ? "Control is being handed back. Try again in a moment."
            : !canAcquire ? "Waiting for a current picture from the computer." : null);
  const lockedWhy = control.enabled ? null : "Focus, Close and Open work once you enable control below.";
  return (
    <Screen
      title="Apps and windows"
      subtitle={profile.name}
      className="windows-screen"
      footer={
        target && (
          <div className="control-footer">
            {control.mine ? (
              <SafetyActions target={target} human onLeave={control.leave} />
            ) : (
              <>
                {!["ready", "paused"].includes(
                  control.view?.input_state ?? "",
                ) && <SafetyActions target={target} compact />}
                {acquireWhy && !pending && <p className="fine windows-why">{acquireWhy}</p>}
                <Button
                  block
                  variant="primary"
                  icon={Hand}
                  disabled={!canAcquire || !!pending}
                  pending={pending === "Requesting control…"}
                  onClick={() => void acquire()}
                >
                  Enable control
                </Button>
              </>
            )}
          </div>
        )
      }
    >
      <ConnectionNote />
      {target ? (
        <>
          <OwnershipStrip
            info={{
              ...ownership(control.view, profile.deviceId, () => "Agent"),
              ...(control.mine
                ? {
                    detail:
                      "Window actions go to this screen while you have control.",
                  }
                : {}),
            }}
          />
          <div className="windows-picture">
            <LivePicture
              target={target}
              canvas={media.canvas} backdrop={media.backdrop} onRegion={media.setRegion}
              picture={media.picture}
              enabled={false}
              send={() => false}
              retry={media.retry}
            />
          </div>
          <p className="fine">
            {control.enabled
              ? "Actions affect the computer’s actual desktop."
              : "Review the live picture and enable control to act. Other active input must be paused first."}
          </p>
        </>
      ) : (
        <Notice tone="warn">
          No display is available to control. You can browse the list,
          but actions need a current desktop picture.
        </Notice>
      )}
      {control.error && <Notice tone="warn">{control.error}</Notice>}
      {error && <Notice tone="warn">{error}</Notice>}
      {pending && (
        <p role="status" className="fine">
          {pending}
        </p>
      )}
      {lastCommand && progress?.stage !== "observed" && (
        <StateLine
          phase={
            lastCommand.phase === "unknown" ||
            progress?.stage === "uncertain" ||
            progress?.stage === "reconciliation_required"
              ? "unknown"
              : lastCommand.phase === "rejected" || progress?.stage === "failed"
                ? "failed"
                : lastCommand.phase === "sending"
                  ? "sending"
                  : "host_accepted"
          }
          at={lastCommand.issuedAt}
          detail={
            progress?.error?.message ??
            lastCommand.error?.message ??
            lastCommand.label
          }
          computer={profile.name}
        />
      )}
      {!receipt && recorded && (
        <Notice title="Recorded on the computer">
          {recorded.receipt.summary} · <Age at={recorded.at} suffix=" ago" />
        </Notice>
      )}
      {receipt && (
        <Notice title="Recorded on the computer">
          {receipt.detail} · <Age at={receipt.requested_at} suffix=" ago" />
        </Notice>
      )}
      <div className="windows-tools">
        <TextField
          label="Find an app or window"
          type="search"
          value={search}
          onChange={setSearch}
        />
        <Button
          icon={RefreshCw}
          disabled={!online || loading}
          pending={loading}
          onClick={() => setRefresh((n) => n + 1)}
        >
          Refresh list
        </Button>
      </div>
      {catalog && (
        <p className="fine">
          Checked <Age at={catalog.observed_at} suffix=" ago" now="just now" />
          {catalog.truncated && " · Partial list"}
        </p>
      )}
      <Section title="Windows">
        {lockedWhy && target && <p className="fine">{lockedWhy}</p>}
        {capability && (
          <CapabilityNote
            what="Window actions"
            availability={capability.windows}
          />
        )}
        {windows.length ? (
          <ul className="windows-list">
            {windows.map((w) => (
              <li key={w.window_id} className="windows-card">
                <div className="windows-title">
                  <AppWindow aria-hidden />
                  <h3>{w.title || "Untitled window"}</h3>
                </div>
                <p className="fine">
                  {[w.app, w.workspace && `Workspace ${w.workspace}`, w.monitor]
                    .filter(Boolean)
                    .join(" · ")}
                </p>
                {w.focused && (
                  <p className="windows-focused">Focused on the computer</p>
                )}
                <div className="windows-actions">
                  <Button
                    disabled={
                      !ready || capability?.windows.state !== "available"
                    }
                    onClick={() =>
                      void act(
                        { action: "focus", window_id: w.window_id },
                        `Focus ${w.title}`,
                      )
                    }
                  >
                    Focus window
                  </Button>
                  <Button
                    variant="quiet"
                    disabled={
                      !ready || capability?.windows.state !== "available"
                    }
                    onClick={() => warn.ask(() => setClosing(w), () => void act(
                      { action: "close", window_id: w.window_id },
                      `Close ${w.title}`,
                    ))}
                  >
                    Close window{warn.more}
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        ) : (
          <Empty
            title={
              loading && !catalog
                ? "Loading windows…"
                : needle
                  ? "No matching windows"
                  : "No windows listed"
            }
          >
            {capability?.windows.state === "available"
              ? "Refresh after opening a window on the computer."
              : "Installed apps are listed below when available."}
          </Empty>
        )}
      </Section>
      <Section title="Installed apps">
        {lockedWhy && target && apps.length > 0 && <p className="fine">{lockedWhy}</p>}
        {capability && (
          <CapabilityNote
            what="Open an app"
            availability={capability.open_apps}
          />
        )}
        {apps.length ? (
          <ul className="windows-list">
            {apps.map((app) => (
              <li key={app.app_id} className="windows-app">
                <span>{app.name}</span>
                <Button
                  disabled={
                    !ready || capability?.open_apps.state !== "available"
                  }
                  aria-label={`Open ${app.name}`}
                  onClick={() =>
                    void act(
                      { action: "open_app", app_id: app.app_id },
                      `Open ${app.name}`,
                    )
                  }
                >
                  Open app
                </Button>
              </li>
            ))}
          </ul>
        ) : (
          <Empty
            title={
              loading && !catalog
                ? "Loading installed apps…"
                : "No matching apps"
            }
          >
            Search by the app’s name or refresh the list.
          </Empty>
        )}
      </Section>
      <Sheet
        urgentExcept={{ targetId: route.targetId }}
        open={!!closing && visible}
        title="Close this window?"
        onClose={() => setClosing(null)}
        footer={
          <Button
            variant="stop"
            block
            disabled={!ready || !closing}
            onClick={() => {
              if (!closing) return;
              warn.agreed();
              void act(
                { action: "close", window_id: closing.window_id },
                `Close ${closing.title}`,
              );
            }}
          >
            Request close
          </Button>
        }
      >
        <p className="windows-close-title">{closing?.title}</p>
        <p>
          The app may prompt about unsaved work. Check the desktop before
          assuming the window has closed.
        </p>
        {warn.check}
        {control.mine && (
          <SafetyActions
            target={target}
            human
            onLeave={() => {
              setClosing(null);
              control.leave();
            }}
          />
        )}
      </Sheet>
    </Screen>
  );
}
