// SPDX-License-Identifier: Apache-2.0
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { announce } from "../design/Announcer";
import { PaneFrame, PaneContext, usePane } from "../shell/pane";
import { setActiveBackPane } from "../design/back";
import { SplitStore, SIDES, otherSide, type Side } from "./split";
import { SplitControls } from "./SplitControl";
import { useSplitLayout } from "./useSplitLayout";
import { useCheckWriters } from "./checkWriters";
import "./split.css";
import { openReport } from "../reports/request";
import { reports } from "../diagnostics/reports";
import { trail } from "../diagnostics/trail";
import { loadPlatform } from "../host";
import type { HostProfile, Platform } from "../host/link";
import type { HostSession } from "../host/session";
import { Button } from "../design/Button";
import { LiveRegions } from "../design/Announcer";
import { Welcome, type OpenedLink } from "../screens/welcome/Welcome";
import { Shell } from "../shell/Shell";
import { PAIRING_SCHEME } from "../brand";
import { AppProvider, type App as AppValue } from "./context";
import { carryOver, keepWork } from "./keptWork";
import { keepOutbox } from "./outbox";
import { prefs } from "./prefs";
import { homeTab, UiStore, type Route, type Tab } from "./ui";
import { Computers } from "./computers";
import { dropTakenName, renameWay, takeName } from "./computerName";
import { describeError } from "./context";
import { toast } from "../design/Toast";
import { PlatformEvents } from "../platform/PlatformEvents";
import { PlatformNotices } from "../platform/errors";
import { keepLaunchDefaults } from "./launchDefaults";
import { Sheet } from "../design/Sheet";
import { useStore } from "../state/store";
import { annotationStore, holdUnanswered } from "../state/annotations";
import { appLock } from "../platform/lock";
import { LockScreen } from "../platform/LockScreen";
import { objectLinkProblem, parseObjectLink, onObjectLink, withPairing, type ObjectLink } from "../platform/links";
import { LinkPreview } from "../platform/LinkPreview";
import { Handoffs, hasRisk, switchRisk } from "../platform/switching";
import { forgetCache } from "../platform/offline";
import { UrgentSlot, type UrgentExcept } from "../design/urgent";
import SafetyStrip from "../screens/control/SafetyStrip";
import { transferManager } from "../screens/transfers/manager";
import { forgetRemovedMarkup, resumeRemovedMarkup } from "../screens/talk/removedMarkup";
import { HandoffBanner, HandoffStops } from "../platform/HandoffBanner";
import { useWarning } from "./warnings";
import { ContributionInbox, CONTRIBUTION_LINK_PREFIX, parseContributionLink } from "../platform/contributionLinks";
import { ContributionDelivery } from "../platform/ContributionDelivery";
import { startWhenShown } from "./signInFeature";
import { TaskAlerts } from "../shell/AlertsOffer";
import { AlertsSetup } from "../shell/AlertsSetup";
import { LoginUseNotices } from "../shell/LoginUseNotices";
import { isActiveTask } from "../state/projection";

const ConnectedReports = lazy(() => import("../reports/ReportsOverlay").then(m => ({ default: m.ConnectedReports })));
const ReportsOverlay = lazy(() => import("../reports/ReportsOverlay").then(m => ({ default: m.ReportsOverlay })));

type Boot = { platform: Platform; profiles: HostProfile[] };
export function App() {
  const [boot, setBoot] = useState<Boot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const start = useCallback(async () => {
    setError(null);
    try { const platform = await loadPlatform(); setBoot({ platform, profiles: await platform.profiles() }); }
    catch (e) { setError(e instanceof Error ? e.message : "The app couldn’t load this device’s computers."); }
  }, []);
  useEffect(() => { void start(); }, [start]);
  return <><LiveRegions />{boot ? <PlatformApp {...boot} /> : <div className="boot boot--error">
    {error ? <><h1 className="boot__title">The app couldn’t start</h1><p>{error}</p><Button variant="primary" onClick={() => void start()}>Try again</Button><Button variant="quiet" onClick={() => openReport({ type: "error" }, { screen: "/startup", error: { code: "startup", message: error } })}>Report</Button><Suspense fallback={null}><ReportsOverlay /></Suspense></> : <p role="status">Opening the app…</p>}
  </div>}</>;
}
/** Where to go: another computer, a route on it, or one of its projects on
 * the tab now shown, as All's project list opens one. */
interface Navigation { profile: HostProfile; route?: Route; pair?: boolean; close?: boolean; project?: { projectId: string; tab: Tab };
  /** Show this computer alone, leaving All, once the switch happens. */
  alone?: boolean;
  /** Done on the computer's interface once it is shown. */
  then?: (ui: UiStore) => void }
function PlatformApp({ platform, profiles: initialProfiles }: Boot) {
  const [profiles, setProfiles] = useState(initialProfiles);
  const contributions = useMemo(() => new ContributionInbox(), []);
  const [split] = useState(() => new SplitStore(initialProfiles, prefs.get().lastProfileId, prefs.get().allComputers === true));
  const workspace = useStore(split, state => state);
  const activeSide = workspace.active;
  const profile = profiles.find(p => p.profileId === workspace.sides[activeSide].profileId) ?? null;
  const layout = useSplitLayout(platform);
  const twoVisible = workspace.open && layout.geometry.eligible;
  const visibleSides = twoVisible ? SIDES : [activeSide];
  const [globalRoot, setGlobalRoot] = useState<HTMLDivElement | null>(null);
  const [alertsLifetime] = useState<object>(() => ({}));
  const [pairing, setPairing] = useState(!profile);
  const [pairingLink, setPairingLink] = useState<OpenedLink | null>(null);
  const [link, setLink] = useState<ObjectLink | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);
  const [navigationBySide, setNavigationBySide] = useState<Partial<Record<Side, Navigation | null>>>({});
  const setNavigation = (navigation: Navigation | null) => setNavigationBySide(s => ({ ...s, [split.get().active]: navigation }));
  const controller = useMemo(() => appLock(platform), [platform]);
  const lock = useStore(controller, (s) => s.locked || s.checking);
  // Whether the header shows every computer at once, remembered as the
  // computer last used is. Only one computer is the same as All.
  const showAll = (on: boolean, side = activeSide) => {
    if (side === activeSide) prefs.set((p) => ({ ...p, allComputers: on }));
    split.showAll(side, on);
  };
  const held = useRef<Handoffs | null>(null);
  const named = useRef<(id: string, name: string) => void>(() => {});
  // One session per paired computer for as long as the app runs.
  const computers = useMemo(() => new Computers(platform, initialProfiles, {
    held: (id) => held.current?.get().some((r) => r.profileId === id) ?? false,
    named: (id, name) => named.current(id, name),
  }), [platform, initialProfiles]);
  const handoffs = useMemo(() => new Handoffs(platform, initialProfiles, (id) => computers.session(id)), [platform, initialProfiles, computers]);
  held.current = handoffs;
  handoffs.profiles = profiles;
  const renamed = (next: HostProfile) => {
    setProfiles((all) => all.map((p) => (p.profileId === next.profileId ? next : p)));
  };
  // A name the computer's owner gave it there becomes this device's name for it.
  named.current = (id, name) => { void platform.renameProfile(id, name).then((next) => { takeName(id, next.name); renamed(next); }, () => {}); };
  useEffect(() => computers.hold(), [computers]);
  // A report about another computer goes through that computer.
  useEffect(() => reports.setSessions((id) => computers.peek(id)), [computers]);
  useEffect(() => { computers.sync(profiles); }, [computers, profiles]);
  // One interface store per computer while the app runs, so what still
  // watches one after a switch away and back, such as removed markup waiting
  // to go, works on what the screens show.
  const uis = useMemo(() => new Map<string, UiStore>(), []);
  const uiFor = (id: string, side: Side = "left") => { let ui = uis.get(id); if (!ui) uis.set(id, (ui = new UiStore(id))); return side === "left" ? ui : ui.fork(side); };
  const forgetUi = (id: string) => { const ui = uis.get(id); uis.delete(id); if (ui) { forgetRemovedMarkup(ui); for (const view of ui.navigationStores()) view.retire(); } };
  // Closed sides do not need a navigation store at all. The canonical work
  // owner still exists when the surviving side is the right one.
  const openSides = workspace.open ? SIDES : [activeSide];
  for (const side of openSides) {
    const id = workspace.sides[side].profileId;
    if (id && profiles.some(p => p.profileId === id)) uiFor(id, side);
  }
  // An open side remembers positions on computers it is not currently showing.
  // Restore those saved editors before shared-work cleanup examines attachments.
  for (const ui of uis.values()) for (const side of openSides) ui.restoreNavigation(side);
  useEffect(() => {
    // Cached histories of closed sides are not editors. A narrowed side stays
    // open, and each open side keeps its per-computer position when switching.
    for (const ui of uis.values()) for (const view of ui.navigationStores())
      view.navigationRetained.set(openSides.some(side => side === view.navigationSide()));
  });
  const shownIds = new Set((pairing || lock ? [] : visibleSides).flatMap(side => workspace.sides[side].profileId ? [workspace.sides[side].profileId!] : []));
  handoffs.activeProfileId = profile?.profileId ?? null;
  handoffs.visibleProfileIds = shownIds;
  useEffect(() => {
    const connected = !pairing && !lock;
    computers.view(connected ? profile?.profileId ?? null : null);
    setActiveBackPane(connected ? activeSide : null);
  }, [computers, profile?.profileId, activeSide, pairing, lock]);
  const announcedSide = useRef(activeSide);
  useEffect(() => {
    if (announcedSide.current !== activeSide && twoVisible) announce(`${activeSide === "left" ? "Left" : "Right"} side active`, "polite");
    announcedSide.current = activeSide;
  }, [activeSide, twoVisible]);
  const previousShown = useRef<ReadonlySet<string>>(new Set());
  useEffect(() => {
    for (const id of previousShown.current) if (!shownIds.has(id)) {
      const computer = profiles.find(p => p.profileId === id), session = computers.peek(id);
      if (computer && session) { const risk = switchRisk(session.state.get(), session.commands.get(), computer.deviceId); if (hasRisk(risk)) handoffs.retain(computer, session, risk); }
    }
    previousShown.current = shownIds;
  });
  const openSplit = () => {
    if (!layout.geometry.eligible || workspace.open || !profile) return;
    const origin = uiFor(profile.profileId, activeSide), target = uiFor(profile.profileId, otherSide(activeSide));
    const state = origin.get(), nextTab = state.tab === "talk" ? "work" : "talk";
    target.set(s => ({ ...s, projectId: state.projectId, taskByProject: { ...state.taskByProject }, taskList: state.taskList, conversing: state.conversing, talkAll: state.talkAll, todoAll: state.todoAll, targetId: state.targetId }));
    target.selectTab(nextTab); target.reset(nextTab);
    split.open();
  };
  useEffect(() => { void reports.setPlatform(platform); }, [platform]);
  useEffect(() => { if (pairing) { trail.record({ type: "route", route: "/welcome" }); trail.record({ type: "connection", state: "unpaired" }); } }, [pairing]);
  useEffect(() => controller.start(), [controller]);
  useEffect(() => { handoffs.restore(); return () => handoffs.dispose(); }, [handoffs]);
  useEffect(() => {
    const receive = (url: string) => {
      if (url.startsWith(CONTRIBUTION_LINK_PREFIX)) {
        const parsed = parseContributionLink(url);
        setLink(null);
        setLinkError(parsed ? null : "This contribution link is incomplete or has been changed. Open it again in Clankerhub.");
        if (parsed) contributions.receive(parsed);
        return;
      }
      const parsed = parseObjectLink(url);
      setLink(parsed); setLinkError(parsed ? null : objectLinkProblem(url));
    };
    // Only `ophio setup`, over adb, opens a pairing link; a pasted one is not an item.
    const native = (url: string) => {
      if (!url.startsWith(`${PAIRING_SCHEME}://pair?`)) return receive(url);
      setPairingLink({ url }); setPairing(true);
    };
    const offNative = platform.onAppLink?.(native), offPaste = onObjectLink(receive);
    return () => { offNative?.(); offPaste(); };
  }, [platform, contributions]);
  const forgetComputer = async (id: string) => {
    const gone = profiles.find((p) => p.profileId === id);
    if (!gone) return;
    // As for the computer on screen: one still confirming that control
    // ended, or with a Stop not yet confirmed, can't be forgotten, unless it
    // refuses this device's key, which nothing here can change.
    const session = computers.peek(id), link = session?.link.get();
    const refused = link?.state === "failed" && ["revoked", "unauthenticated", "not_paired"].includes(link.reason);
    const unsettled = handoffs.get().some((r) => r.profileId === id)
      || (session ? hasRisk(switchRisk(session.state.get(), session.commands.get(), gone.deviceId)) : false);
    if (!refused && unsettled) throw new Error("Leave control and resolve pending Stop requests before forgetting this computer.");
    await platform.forget(id); dropTakenName(id); forgetCache(id); computers.forget(id); handoffs.dismiss(id); keepWork(gone); forgetUi(id);
    const rest = profiles.filter((p) => p.profileId !== id); setProfiles(rest);
    split.reconcile(rest); if (!rest.length) setPairing(true);
  };
  const choose = (next: HostProfile, side: Side = activeSide) => {
    if (side === activeSide) prefs.set((p) => ({ ...p, lastProfileId: next.profileId }));
    split.choose(side, next.profileId); setPairing(false); setPairingLink(null);
  };
  const returnTo = (id: string, route?: Route, then?: Navigation["then"]) => {
    const next = profiles.find((p) => p.profileId === id);
    if (!next) return;
    setNavigation({ profile: next, route, then });
    if (pairing) choose(next);
  };
  const PairingSafety = useMemo(() => function PairingStops() {
    return <HandoffStops handoffs={handoffs} current="" />;
  }, [handoffs]);
  const linked = link && (profiles.find((p) => p.profileId === profile?.profileId && p.hostId === link.hostId) ?? profiles.find((p) => p.hostId === link.hostId));
  const extras = () => !lock && <><PlatformNotices />
      {link && <LinkPreview link={link} profiles={profiles} platform={platform} existingSession={linked ? computers.session(linked.profileId) : undefined} onClose={() => setLink(null)} onOpen={(next, route) => {
        setLink(null);
        if (profile && !pairing) setNavigation({ profile: next, route });
        else { choose(next); setNavigation({ profile: next, route }); }
      }} />}
      <Sheet open={linkError !== null} title="Can’t open link" onClose={() => setLinkError(null)}><p>{linkError}</p></Sheet>
  </>;
  return <>
    <PlatformEvents platform={platform} profileId={profile?.profileId ?? null} onOpen={(route) => {
      const computer = profiles.find((p) => p.profileId === route.profileId);
      if (!computer) { setLinkError("This computer is no longer paired with this device."); return; }
      // "Tap to sign in again": the sign-in starts once its screen shows, if still needed.
      if (route.kind === "sign_in") startWhenShown(computer.profileId, route.accountId, "if_signed_out");
      // The app's own alerts open straight from what the phone already holds;
      // the attention screen checks the item as the computer catches up. Another
      // computer's alert switches through the usual leave-control check.
      setNavigation({ profile: computer, route: route.kind === "agent_updates" ? { screen: "settings", section: "settings-agents" }
        : route.kind === "app_update" || route.kind === "new_version" ? { screen: "settings", section: "settings-versions" }
        : route.kind === "report" ? { screen: "settings", section: "settings-reports", reportId: route.reportId }
        : route.kind === "needs_you" ? { screen: "needs_you", projectId: route.projectId }
        : route.kind === "sign_in" ? { screen: "sign_in", accountId: route.accountId } : { screen: "attention", attentionId: route.attentionId } });
      if (pairing) choose(computer);
    }} />
    {lock && <LockScreen controller={controller} />}
    {!lock && pairing && <UrgentSlot.Provider value={PairingSafety}><div className="connected-frame">
      {extras()}
      <HandoffBanner handoffs={handoffs} current="" onReturn={returnTo} />
      <Suspense fallback={null}><ReportsOverlay /></Suspense>
      <Welcome platform={platform} link={pairingLink} onCancel={profile ? () => { setPairing(false); setPairingLink(null); } : undefined} onPaired={async (next) => {
        const replaced = profiles.filter((p) => p.hostId === next.hostId && p.profileId !== next.profileId);
        for (const old of replaced) {
          handoffs.dismiss(old.profileId); computers.forget(old.profileId); forgetCache(old.profileId); forgetUi(old.profileId);
        }
        await carryOver(next, replaced.map(p => p.profileId));
        const paired = withPairing(profiles, next);
        setProfiles((all) => withPairing(all, next));
        const replacedIds = new Set(replaced.map(p => p.profileId));
        for (const side of SIDES) if (replacedIds.has(split.get().sides[side].profileId ?? "")) split.choose(side, next.profileId);
        choose(next); split.reconcile(paired);
      }} />
    </div></UrgentSlot.Provider>}
    {[...uis.entries()].filter(([id]) => profiles.some(p => p.profileId === id)).map(([id, ui]) => <ComputerWork key={id} platform={platform} profile={profiles.find(p => p.profileId === id)!} ui={ui} computers={computers} />)}
    {profile && !pairing && <div className="split-frame" style={layout.geometry.safeHorizontal ? { height: layout.geometry.safeHorizontal.height, marginTop: layout.geometry.safeHorizontal.top, maxHeight: `calc(100dvh - ${layout.geometry.safeHorizontal.top}px)` } : undefined}>
      <div ref={setGlobalRoot} className="split-global" />
      <div className={`split-space${twoVisible ? " split-space--open" : ""}`} style={{ gridTemplateColumns: layout.geometry.fold || twoVisible ? `${layout.geometry.left}px ${layout.geometry.gap}px minmax(0, 1fr)` : undefined }}>
        {SIDES.map(side => {
          const computer = profiles.find(p => p.profileId === workspace.sides[side].profileId);
          if (!computer || (!workspace.open && side !== activeSide)) return null;
          const visible = visibleSides.includes(side);
          const segmented = layout.geometry.fold;
          return <div key={side} className="split-slot" style={{ display: visible ? undefined : "none", gridColumn: twoVisible || segmented ? (side === "left" ? 1 : 3) : 1 }}>
            <PaneFrame id={side} label={side === "left" ? "Left side" : "Right side"} active={side === activeSide} visible={visible && !lock} split={twoVisible} onActivate={() => split.activate(side)}>
              <Connected key={computer.profileId} platform={platform} profile={computer} ui={uiFor(computer.profileId, side)} profiles={profiles} locked={lock}
                computers={computers} all={workspace.sides[side].all && profiles.length > 1} onShowAll={on => showAll(on, side)} onForgetOther={forgetComputer} uiFor={id => uiFor(id, side)}
                handoffs={handoffs} contributions={contributions} navigation={navigationBySide[side] ?? null} children={extras} onNavigated={() => setNavigationBySide(s => ({ ...s, [side]: null }))} onSwitch={(next, route, project, then) => {
                  choose(next, side);
                  if (route || project || then) setNavigationBySide(s => ({ ...s, [side]: { profile: next, route, project, then } }));
                }} onReturn={returnTo} onPairAnother={() => setPairing(true)} onRenamed={renamed} onForgotten={(id) => {
                  computers.forget(id); handoffs.dismiss(id); forgetUi(id);
                  const rest = profiles.filter(p => p.profileId !== id); setProfiles(rest); split.reconcile(rest); setPairing(!rest.length);
                }}
                globalRoot={globalRoot} active={side === activeSide} visible={visible && !lock} shownIds={shownIds}
                alertsLifetime={alertsLifetime} notices={visible && visibleSides.find(candidate => workspace.sides[candidate].profileId === computer.profileId) === side}
                splitControls={{ side, available: layout.geometry.eligible, opened: workspace.open, twoVisible, open: openSplit, close: () => split.close(side) }} />
            </PaneFrame>
          </div>;
        })}
        {layout.geometry.fold && !workspace.open && <div className="split-invitation" style={{ gridColumn: activeSide === "left" ? 3 : 1 }}>
          <p>Another place for your work</p>{layout.geometry.eligible && <Button onClick={openSplit}>Open split view</Button>}
        </div>}
        {(layout.geometry.fold || twoVisible) && <div className="split-divider" aria-hidden="true" style={{ gridColumn: 2 }} />}
      </div>
    </div>}
  </>;
}
interface ConnectedProps {
  children?: () => ReactNode; platform: Platform; profile: HostProfile; ui: UiStore; profiles: HostProfile[]; locked: boolean; handoffs: Handoffs;
  computers: Computers; all: boolean; onShowAll(on: boolean): void; onForgetOther(id: string): Promise<void>; uiFor(id: string): UiStore;
  contributions: ContributionInbox;
  navigation: Navigation | null; onNavigated(): void; onSwitch(p: HostProfile, route?: Route, project?: Navigation["project"], then?: Navigation["then"]): void; onPairAnother(): void; onRenamed(p: HostProfile): void; onForgotten(id: string): void;
  onReturn(id: string, route?: Route, then?: Navigation["then"]): void;
  globalRoot: HTMLElement | null; active: boolean; visible: boolean; shownIds: ReadonlySet<string>;
  alertsLifetime: object; notices: boolean;
  splitControls: import("./SplitControl").SplitControlsValue;
}
function Connected(props: ConnectedProps) {
  const { computers, profile, visible } = props;
  const current = useStore(computers, state => state.get(profile.profileId));
  // Keep hidden screen state mounted while its connection is parked. Only the
  // visibility effect or Computers' foreground handler may reconnect it; a
  // background resize must not do so merely by rendering the hidden side.
  const previous = useRef(current);
  if (current) previous.current = current;
  useEffect(() => {
    if (!visible) return;
    // A newly paired profile can mount before the parent's sync effect runs.
    computers.session(profile.profileId, profile);
    return computers.show(profile.profileId);
  }, [computers, profile, visible]);
  const session = current ?? previous.current;
  return session ? <ConnectedPane {...props} session={session} /> : null;
}
function ConnectedPane({ children, platform, profile, ui, profiles, locked, computers, all, onShowAll, onForgetOther, uiFor, handoffs, contributions, navigation, onNavigated, onSwitch, onReturn, onPairAnother, onRenamed, onForgotten, globalRoot, active, shownIds, splitControls, alertsLifetime, notices, session }: ConnectedProps & { session: HostSession }) {
  const [pending, setPending] = useState<Navigation | null>(null);
  const [leaving, setLeaving] = useState(false);
  const warn = useWarning("computer.leave");
  const requested = useRef<Navigation | null>(null);
  useEffect(() => () => { ui.cancelTakeover(); ui.persistNow(); }, [ui]);
  useEffect(() => active ? reports.context(session, ui) : undefined, [active, session, ui]);
  useEffect(() => { if (locked) { ui.cancelTakeover(); requested.current = null; setPending(null); onNavigated(); } }, [locked, ui]); // Opening the app never completes an interrupted confirmation.
  const perform = (next: Navigation) => {
    if (next.close) { onNavigated(); splitControls.close(); return; }
    if (next.pair) { onNavigated(); onPairAnother(); return; }
    if (next.alone) onShowAll(false);
    if (next.profile.profileId === profile.profileId) {
      if (next.project) { ui.selectProject(next.project.projectId); ui.reset(next.project.tab); }
      if (next.route) ui.push(next.route, homeTab(next.route));
      next.then?.(ui); onNavigated();
    } else { onSwitch(next.profile, next.route, next.project, next.then); }
  };
  const riskyComputers = () => [...shownIds].flatMap(id => {
    const computer = profiles.find(p => p.profileId === id), current = computers.peek(id);
    if (!computer || !current) return [];
    const risk = switchRisk(current.state.get(), current.commands.get(), computer.deviceId);
    return hasRisk(risk) ? [{ computer, session: current, risk }] : [];
  });
  const request = (next: Navigation) => {
    if (next.pair) {
      // Pairing is app-wide: it hides both sides, not just the requesting one.
      if (riskyComputers().length) warn.ask(() => setPending(next), () => leave(next)); else perform(next);
      return;
    }
    if (next.profile.profileId === profile.profileId && !next.close) { perform(next); return; }
    const sameElsewhere = splitControls.twoVisible && computers.visibleCount(profile.profileId) > 1;
    if (sameElsewhere) { perform(next); return; }
    const risk = switchRisk(session.state.get(), session.commands.get(), profile.deviceId);
    if (hasRisk(risk)) warn.ask(() => setPending(next), () => leave(next)); else perform(next);
  };
  const leave = (next: Navigation) => {
    const targets = next.pair ? riskyComputers() : [{ computer: profile, session, risk: switchRisk(session.state.get(), session.commands.get(), profile.deviceId) }];
    for (const target of targets) handoffs.retain(target.computer, target.session, target.risk);
    ui.cancelTakeover(); ui.persistNow(); setLeaving(true);
    void Promise.all(targets.map(target => handoffs.release(target.computer.profileId))).finally(() => {
      setLeaving(false); setPending(null);
      // An authentication boundary cancels navigation but keeps unresolved control visible.
      if (!appLock(platform).get().locked) perform(next);
    });
  };
  useEffect(() => {
    if (!navigation || locked || requested.current === navigation) return;
    requested.current = navigation; request(navigation);
  }, [navigation, locked]);
  const app: AppValue = {
    platform, profile, profiles, session, ui, computers, all, computerServices: false,
    switchTo(id, route) {
      const next = profiles.find((p) => p.profileId === id);
      // Choosing a computer shows that computer alone.
      if (next) request({ profile: next, route, alone: true });
    },
    showAll() { onShowAll(true); },
    openProject(id, projectId, then) {
      const next = profiles.find((p) => p.profileId === id);
      if (!next) return;
      if (id === profile.profileId) { ui.selectProject(projectId); then?.(); return; }
      request({ profile: next, project: { projectId, tab: ui.get().tab }, then: then && (() => then()) });
    },
    pairAnother: () => request({ profile, pair: true }),
    async rename(id, name) {
      const computer = profiles.find((p) => p.profileId === id);
      if (!computer) throw new Error("This computer is no longer paired with this device.");
      const other = computers.peek(id);
      const way = other ? renameWay(other.state.get(), other.link.get()) : "phone";
      if (way === "wait") throw new Error(`Connect to ${computer.name} to rename it.`);
      // A name given on this device alone stays, whatever the computer says.
      if (way === "phone") { const next = await platform.renameProfile(id, name); dropTakenName(id); onRenamed(next); return; }
      // The computer keeps the name, so every device paired with it shows
      // it. The name changes here at once, stays until the computer says
      // it, and goes back if it refuses.
      computers.expectName(id, name);
      onRenamed({ ...computer, name });
      void other!.command({ command: "rename_computer", name }, { label: `Rename ${computer.name}` }).then(
        () => platform.renameProfile(id, name).then((next) => { takeName(id, next.name); onRenamed(next); }, () => {}),
        (e: unknown) => {
          computers.settleName(id);
          onRenamed(computer);
          toast(`${computer.name} wasn’t renamed. ${describeError(e, computer.name)}`, { tone: "stop" });
        },
      );
    },
    forgetOther: onForgetOther,
    uiOf: uiFor,
    visit(id, act) {
      const next = profiles.find((p) => p.profileId === id);
      if (next) request({ profile: next, then: act });
    },
    async forget() {
      // A computer that refuses this device's key can't be asked to end
      // control or confirm Stop, and keeping the key can't change that.
      const link = session.link.get();
      const refused = link.state === "failed" && ["revoked", "unauthenticated", "not_paired"].includes(link.reason);
      if (!refused && hasRisk(switchRisk(session.state.get(), session.commands.get(), profile.deviceId))) throw new Error("Leave control and resolve pending Stop requests before forgetting this computer.");
      await platform.forget(profile.profileId); dropTakenName(profile.profileId); forgetCache(profile.profileId); session.stop(); ui.retire(); keepWork(profile); onForgotten(profile.profileId);
    },
  };
  const Urgent = useMemo(() => function CombinedSafety(props: { dismiss?: () => void; except?: UrgentExcept }) {
    const pane = usePane();
    return <><SafetyStrip {...props} />{!pane?.split && <HandoffStops handoffs={handoffs} current={shownIds} />}</>;
  }, [handoffs, profile.profileId, [...shownIds].join("|")]);
  const GlobalUrgent = () => <>
    {[...shownIds].map(id => {
      const computer = profiles.find(p => p.profileId === id), current = computers.peek(id);
      if (!computer || !current) return null;
      const other = id === profile.profileId ? app : { ...app, profile: computer, session: current, ui: uiFor(id), all: false };
      return <AppProvider key={id} app={other}><SafetyStrip /></AppProvider>;
    })}
    <HandoffStops handoffs={handoffs} current={shownIds} />
  </>;
  const globalContent = active && !locked && <PaneContext.Provider value={null}><UrgentSlot.Provider value={GlobalUrgent}>
    {children?.()}
    <AlertsSetup lifetime={alertsLifetime} />
    <HandoffBanner handoffs={handoffs} current={shownIds} onReturn={onReturn} />
    <ContributionDelivery inbox={contributions} locked={locked} />
    <Suspense fallback={null}><ConnectedReports /></Suspense>
  </UrgentSlot.Provider></PaneContext.Provider>;
  return <AppProvider app={app}><SplitControls.Provider value={{ ...splitControls, close: () => request({ profile, close: true }) }}><UrgentSlot.Provider value={Urgent}>
    {globalRoot && globalContent && createPortal(globalContent, globalRoot)}
    {!locked && <div className="connected-frame">
    {notices && <><TaskAlerts /><LoginUseNotices onOpenTask={task => onReturn(profile.profileId, undefined, target => {
      if (isActiveTask(task)) target.openTask(task.project_id, task.task_id);
      else target.push({ screen: "receipt", taskId: task.task_id });
    })} /></>}
    <Shell />
    <Sheet open={pending !== null} title={pending?.pair ? "Leave control before pairing?" : `Leave ${profile.name}?`} onClose={() => { if (!leaving) { setPending(null); onNavigated(); } }} footer={<Button variant="primary" pending={leaving} onClick={() => {
      if (!pending) return;
      warn.agreed(); leave(pending);
    }}>{pending?.pair ? "Leave control and pair" : "Leave control and switch"}</Button>}>
      <p>{pending?.pair ? "Pairing temporarily hides both sides. Leave control on all visible computers before continuing?" : `You still have control or an unresolved request on ${profile.name}. Leave control and switch?`}</p>
      <p>{pending?.pair ? "Your drafts stay with their computer. If a computer doesn’t confirm, its Stop and retry controls stay available. A private step still needs its own review." : `Your drafts stay with ${profile.name}. If the computer doesn’t confirm, its Stop and retry controls stay available. A private step still needs its own review.`}</p>
      {(pending?.pair ? riskyComputers().map(target => target.computer) : [profile]).map(computer => <Button key={computer.profileId} variant="stop-outline" onClick={() => {
        const current = computers.session(computer.profileId);
        const risk = switchRisk(current.state.get(), current.commands.get(), computer.deviceId);
        handoffs.retain(computer, current, risk); void handoffs.stop(computer.profileId);
      }}>Stop on {computer.name}</Button>)}
      {warn.check}
    </Sheet>
  </div>}</UrgentSlot.Provider></SplitControls.Provider></AppProvider>;
}

/** These effects belong to a computer, never to either navigation side. */
function ComputerWork({ platform, profile, ui, computers }: { platform: Platform; profile: HostProfile; ui: UiStore; computers: Computers }) {
  // Observing shared work must not reconnect a deliberately parked computer
  // during render. Connection ownership remains with Computers and shown sides.
  const session = useStore(computers, state => state.get(profile.profileId));
  useEffect(() => platform.outbox ? keepOutbox(platform.outbox, profile.profileId, profile.name, ui) : undefined, [platform, profile.profileId, profile.name, ui]);
  useEffect(() => { const hold = () => holdUnanswered(annotationStore(profile.profileId), ui); hold(); return ui.subscribe(hold); }, [profile.profileId, ui]);
  useEffect(() => resumeRemovedMarkup(ui, annotationStore(profile.profileId)), [profile.profileId, ui]);
  return session ? <ConnectedComputerWork platform={platform} profile={profile} ui={ui} session={session} /> : null;
}
function ConnectedComputerWork({ platform, profile, ui, session }: { platform: Platform; profile: HostProfile; ui: UiStore; session: HostSession }) {
  useCheckWriters(session);
  useEffect(() => reports.watch(session), [session]);
  useEffect(() => keepLaunchDefaults(session), [session]);
  useEffect(() => { transferManager({ platform, profile, session }); }, [platform, profile.profileId, session]);
  useEffect(() => {
    if (import.meta.env.MODE !== "sim") return;
    let stopped = false;
    void import("../dev/sim/reportLoader").then(m => { if (!stopped) return m.loadReportFromQuery(session, ui); }).catch(() => {});
    return () => { stopped = true; };
  }, [session, ui]);
  return null;
}
