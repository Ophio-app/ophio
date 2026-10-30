// SPDX-License-Identifier: Apache-2.0
// A deadline the phone gives a command must mean the same on the computer
// when their clocks disagree, so the computer counts it from arrival.
import { afterEach, expect, it, vi } from "vitest";
import { createSimPlatform } from "../dev/sim/platform";
import type { CommandEnvelope } from "../protocol/types";
import { commandResult } from "./commandResult";
import { HostSession } from "./session";

let session: HostSession;
afterEach(() => { session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

for (const [skew, label] of [[-10_000, "10 s behind"], [10_000, "10 s ahead of"]] as const) {
  it(`voice control starts and takes a command with the phone's clock ${label} the computer's`, async () => {
    localStorage.clear(); vi.useFakeTimers();
    const platform = createSimPlatform();
    const profile = (await platform.profiles())[0]!;
    session = new HostSession(profile.profileId, platform.control(profile.profileId), () => Date.now() + skew);
    session.start(); await vi.advanceTimersByTimeAsync(900);
    const host = window.__sim!.host;
    host.voice.link(host);
    const sent = vi.spyOn(host, "command");
    const target = host.targets[0]!;

    // As Voice and VoiceSession send them.
    const start = session.command({ command: "voice", operation: { operation: "start", target: "browser", target_id: target.target_id, geometry_revision: target.geometry.revision, dry_run: false, disclosure_accepted: true } }, { label: "Start voice control", expiresInMs: 10000 });
    await vi.advanceTimersByTimeAsync(1000);
    const started = await commandResult(session, await start);
    expect(started?.kind).toBe("voice_session");
    if (started?.kind !== "voice_session") return;
    const said = session.command({ command: "voice", operation: { operation: "transcript", session_id: started.session.session_id, utterance_id: "utterance", revision: 1, text: "sign in", final_segment: true } }, { label: "Voice command", expiresInMs: 10000 });
    await vi.advanceTimersByTimeAsync(1000);
    const applied = await commandResult(session, await said);
    expect(applied?.kind === "voice_session" && applied.session.steps.length).toBe(1);

    // The computer sets each deadline on its own clock from this time to live.
    for (const [envelope] of sent.mock.calls as [CommandEnvelope][]) expect(envelope.expires_in_ms).toBe(10000);
  });
}
