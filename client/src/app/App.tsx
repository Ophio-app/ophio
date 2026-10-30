// SPDX-License-Identifier: Apache-2.0
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { openReport } from "../reports/request";
import { reports } from "../diagnostics/reports";
import { trail } from "../diagnostics/trail";
import { loadPlatform } from "../host";
import type { HostProfile, Platform } from "../host/link";
import { HostSession } from "../host/session";
import { Button } from "../design/Button";
import { LiveRegions } from "../design/Announcer";
import { Welcome, type OpenedLink } from "../screens/welcome/Welcome";
import { Shell } from "../shell/Shell";
import { PAIRING_SCHEME } from "../brand";
import { AppProvider, type App as AppValue } from "./context";
import { carryOver, keepWork } from "./keptWork";
import { keepOutbox } from "./outbox";
import { prefs } from "./prefs";
import { homeTab, UiStore, type Route } from "./ui";
import { PlatformEvents } from "../platform/PlatformEvents";
import { PlatformNotices } from "../platform/errors";
import { watchAttention } from "../platform/notifications";
import { keepLaunchDefaults } from "./launchDefaults";
import { Sheet } from "../design/Sheet";
import { useStore } from "../state/store";
import { annotationStore, holdUnanswered } from "../state/annotations";
import { appLock } from "../platform/lock";
import { LockScreen } from "../platform/LockScreen";
import { objectLinkProblem, parseObjectLink, onObjectLink, withPairing, type ObjectLink } from "../platform/links";
import { LinkPreview } from "../platform/LinkPreview";
import { Handoffs, hasRisk, switchRisk } from "../platform/switching";
import { cachedState, cacheObservations, forgetCache } from "../platform/offline";
import { UrgentSlot, type UrgentExcept } from "../design/urgent";
import SafetyStrip from "../screens/control/SafetyStrip";
import { transferManager } from "../screens/transfers/manager";
import { forgetRemovedMarkup, resumeRemovedMarkup } from "../screens/talk/removedMarkup";
import { HandoffBanner, HandoffStops } from "../platform/HandoffBanner";
import { useWarning } from "./warnings";
import { ContributionInbox, CONTRIBUTION_LINK_PREFIX, parseContributionLink } from "../platform/contributionLinks";
import { ContributionDelivery } from "../platform/ContributionDelivery";

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
interface Navigation { profile: HostProfile; route?: Route; pair?: boolean }
function PlatformApp({ platform, profiles: initialProfiles }: Boot) {
  const [profiles, setProfiles] = useState(initialProfiles);
  const contributions = useMemo(() => new ContributionInbox(), []);
  const [profile, setProfile] = useState(() => initialProfiles.find((p) => p.profileId === prefs.get().lastProfileId) ?? initialProfiles[0] ?? null);
  const [pairing, setPairing] = useState(!profile);
  const [pairingLink, setPairingLink] = useState<OpenedLink | null>(null);
  const [link, setLink] = useState<ObjectLink | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);
  const [navigation, setNavigation] = useState<Navigation | null>(null);
  const controller = useMemo(() => appLock(platform), [platform]);
  const lock = useStore(controller, (s) => s.locked || s.checking);
  const handoffs = useMemo(() => new Handoffs(platform, initialProfiles), [platform, initialProfiles]);
  handoffs.profiles = profiles;
  // One interface store per computer while the app runs, so what still
  // watches one after a switch away and back, such as removed markup waiting
  // to go, works on what the screens show.
  const uis = useMemo(() => new Map<string, UiStore>(), []);
  const uiFor = (id: string) => { let ui = uis.get(id); if (!ui) uis.set(id, (ui = new UiStore(id))); return ui; };
  const forgetUi = (id: string) => { const ui = uis.get(id); uis.delete(id); if (ui) { forgetRemovedMarkup(ui); ui.retire(); } };
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
  useEffect(() => {
    if (navigation) return;
    for (const [id, session] of handoffs.sessions) {
      if (id !== profile?.profileId && !handoffs.owns(session)) { session.stop(); handoffs.sessions.delete(id); }
    }
  }, [navigation, profile, handoffs]);
  const choose = (next: HostProfile) => {
    prefs.set((p) => ({ ...p, lastProfileId: next.profileId })); setProfile(next); setPairing(false); setPairingLink(null);
  };
  const returnTo = (id: string, route?: Route) => {
    const next = profiles.find((p) => p.profileId === id);
    if (!next) return;
    setNavigation({ profile: next, route });
    if (pairing) choose(next);
  };
  const PairingSafety = useMemo(() => function PairingStops() {
    return <HandoffStops handoffs={handoffs} current="" />;
  }, [handoffs]);
  const extras = (active?: HostSession) => !lock && <><PlatformNotices />
      {link && <LinkPreview link={link} profiles={profiles} platform={platform} existingSession={active && profiles.some((p) => p.profileId === active.profileId && p.hostId === link.hostId) ? active : handoffs.sessions.get(profiles.find((p) => p.hostId === link.hostId)?.profileId ?? "")} onClose={() => setLink(null)} onOpen={(next, route, previewSession) => {
        handoffs.sessions.set(next.profileId, previewSession);
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
      // The app's own alerts open straight from what the phone already holds;
      // the attention screen checks the item as the computer catches up. Another
      // computer's alert switches through the usual leave-control check.
      setNavigation({ profile: computer, route: route.kind === "app_update" ? { screen: "settings", section: "settings-versions" }
        : route.kind === "report" ? { screen: "settings", section: "settings-reports", reportId: route.reportId }
        : route.kind === "needs_you" ? { screen: "needs_you", projectId: route.projectId } : { screen: "attention", attentionId: route.attentionId } });
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
          handoffs.dismiss(old.profileId); forgetCache(old.profileId); forgetUi(old.profileId);
        }
        await carryOver(next, replaced.map(p => p.profileId));
        setProfiles((all) => withPairing(all, next)); choose(next);
      }} />
    </div></UrgentSlot.Provider>}
    {profile && !pairing && <Connected key={profile.profileId} platform={platform} profile={profile} ui={uiFor(profile.profileId)} profiles={profiles} locked={lock}
      handoffs={handoffs} contributions={contributions} navigation={navigation} children={extras} onNavigated={() => setNavigation(null)} onSwitch={(next, route) => { choose(next); if (route) setNavigation({ profile: next, route }); }} onReturn={returnTo}
      onPairAnother={() => setPairing(true)} onRenamed={(next) => {
        setProfiles((all) => all.map((p) => (p.profileId === next.profileId ? next : p)));
        setProfile((current) => (current?.profileId === next.profileId ? next : current));
      }} onForgotten={(id) => {
        handoffs.dismiss(id); forgetUi(id);
        const rest = profiles.filter((p) => p.profileId !== id); setProfiles(rest); setProfile(rest[0] ?? null); setPairing(!rest.length);
      }} />}
  </>;
}
function Connected({ children, platform, profile, ui, profiles, locked, handoffs, contributions, navigation, onNavigated, onSwitch, onReturn, onPairAnother, onRenamed, onForgotten }: {
  children?: (session: HostSession) => ReactNode; platform: Platform; profile: HostProfile; ui: UiStore; profiles: HostProfile[]; locked: boolean; handoffs: Handoffs;
  contributions: ContributionInbox;
  navigation: Navigation | null; onNavigated(): void; onSwitch(p: HostProfile, route?: Route): void; onPairAnother(): void; onRenamed(p: HostProfile): void; onForgotten(id: string): void;
  onReturn(id: string, route?: Route): void;
}) {
  const session = useMemo(() => {
    const retained = handoffs.sessions.get(profile.profileId);
    if (retained) return retained;
    const next = new HostSession(profile.profileId, platform.control(profile.profileId));
    const cached = cachedState(profile); if (cached) next.state.set(cached);
    return next;
  }, [platform, profile.profileId, handoffs]);
  const [pending, setPending] = useState<Navigation | null>(null);
  const [leaving, setLeaving] = useState(false);
  const warn = useWarning("computer.leave");
  const requested = useRef<Navigation | null>(null);
  useEffect(() => {
    handoffs.activeProfileId = profile.profileId;
    // A handoff kept for this computer from before a restart watches this
    // connection; this effect runs before the app restores kept handoffs.
    if (!handoffs.sessions.has(profile.profileId) && handoffs.get().some((r) => r.profileId === profile.profileId)) handoffs.attach(profile.profileId, session);
    // Returning to the app is when a person looks: try now, not after the wait
    // between attempts, and show what changed meanwhile in one step.
    const offForeground = platform.onForeground?.(() => { if (session.link.get().state === "offline") session.reconnect(); else session.foreground(); });
    session.start();
    return () => {
      // A takeover asked for here does not wait for this computer's return.
      offForeground?.(); ui.cancelTakeover(); ui.persistNow();
      if (!handoffs.owns(session)) session.stop();
    };
  }, [session, platform, handoffs, profile.profileId, ui]);
  useEffect(() => reports.attach(session, ui), [session, ui]);
  useEffect(() => {
    if (import.meta.env.MODE !== "sim") return;
    let stopped = false;
    void import("../dev/sim/reportLoader").then(m => { if (!stopped) return m.loadReportFromQuery(session, ui); }).catch(() => {});
    return () => { stopped = true; };
  }, [session, ui]);
  useEffect(() => cacheObservations(profile, session), [profile, session]);
  useEffect(() => watchAttention(platform, session, profile.name), [platform, session, profile.name]);
  // Work an agent starts on this computer begins with this phone's launch flags.
  useEffect(() => keepLaunchDefaults(session), [session]);
  useEffect(() => platform.outbox ? keepOutbox(platform.outbox, profile.profileId, profile.name, ui) : undefined, [platform, profile.profileId, profile.name, ui]);
  // Every unanswered send keeps its markup's letters, including one saved by
  // an earlier version or put back from the outbox.
  useEffect(() => {
    const hold = () => holdUnanswered(annotationStore(profile.profileId), ui);
    hold();
    return ui.subscribe(hold);
  }, [profile.profileId, ui]);
  // Markup removed before the app last closed is still deleted once let go,
  // or becomes the message's again when changed.
  useEffect(() => resumeRemovedMarkup(ui, annotationStore(profile.profileId)), [profile.profileId, ui]);
  useEffect(() => { if (locked) { ui.cancelTakeover(); requested.current = null; setPending(null); onNavigated(); } }, [locked, ui]); // Opening the app never completes an interrupted confirmation.
  const perform = (next: Navigation) => {
    if (next.pair) { onNavigated(); onPairAnother(); return; }
    if (next.profile.profileId === profile.profileId) {
      if (next.route) ui.push(next.route, homeTab(next.route)); onNavigated();
    } else { onSwitch(next.profile, next.route); }
  };
  const request = (next: Navigation) => {
    if (next.profile.profileId === profile.profileId && !next.pair) { perform(next); return; }
    const risk = switchRisk(session.state.get(), session.commands.get(), profile.deviceId);
    if (hasRisk(risk)) warn.ask(() => setPending(next), () => leave(next)); else perform(next);
  };
  const leave = (next: Navigation) => {
    const risk = switchRisk(session.state.get(), session.commands.get(), profile.deviceId);
    handoffs.retain(profile, session, risk); ui.cancelTakeover(); ui.persistNow(); setLeaving(true);
    void handoffs.release(profile.profileId).finally(() => {
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
    platform, profile, profiles, session, ui,
    switchTo(id, route) { const next = profiles.find((p) => p.profileId === id); if (next) request({ profile: next, route }); },
    pairAnother: () => request({ profile, pair: true }),
    async rename(id, name) { onRenamed(await platform.renameProfile(id, name)); },
    async forget() {
      // A computer that refuses this device's key can't be asked to end
      // control or confirm Stop, and keeping the key can't change that.
      const link = session.link.get();
      const refused = link.state === "failed" && ["revoked", "unauthenticated", "not_paired"].includes(link.reason);
      if (!refused && hasRisk(switchRisk(session.state.get(), session.commands.get(), profile.deviceId))) throw new Error("Leave control and resolve pending Stop requests before forgetting this computer.");
      await platform.forget(profile.profileId); forgetCache(profile.profileId); session.stop(); ui.retire(); keepWork(profile); onForgotten(profile.profileId);
    },
  };
  // Uploads their connection interrupted continue on this connection, whichever screen is open.
  useEffect(() => { transferManager(app); }, [session]);
  const Urgent = useMemo(() => function CombinedSafety(props: { dismiss?: () => void; except?: UrgentExcept }) {
    return <><SafetyStrip {...props} /><HandoffStops handoffs={handoffs} current={profile.profileId} /></>;
  }, [handoffs, profile.profileId]);
  return <AppProvider app={app}><UrgentSlot.Provider value={Urgent}><ContributionDelivery inbox={contributions} locked={locked} />{!locked && <div className="connected-frame">
    {children?.(session)}
    <HandoffBanner handoffs={handoffs} current={profile.profileId} onReturn={onReturn} />
    <Shell />
    <Suspense fallback={null}><ConnectedReports /></Suspense>
    <Sheet open={pending !== null} title={`Leave ${profile.name}?`} onClose={() => { if (!leaving) { setPending(null); onNavigated(); } }} footer={<Button variant="primary" pending={leaving} onClick={() => {
      if (!pending) return;
      warn.agreed(); leave(pending);
    }}>Leave control and switch</Button>}>
      <p>You still have control or an unresolved request on {profile.name}. Leave control and switch?</p>
      <p>Your drafts stay with {profile.name}. If the computer doesn’t confirm, its Stop and retry controls stay available. A private step still needs its own review.</p>
      <Button variant="stop-outline" onClick={() => {
        const risk = switchRisk(session.state.get(), session.commands.get(), profile.deviceId);
        handoffs.retain(profile, session, risk); void handoffs.stop(profile.profileId);
      }}>Stop on {profile.name}</Button>
      {warn.check}
    </Sheet>
  </div>}</UrgentSlot.Provider></AppProvider>;
}
