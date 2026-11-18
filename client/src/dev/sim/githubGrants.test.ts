// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import type { GithubAction, Grants, ServerFrame } from "../../protocol/types";
import { allGrants } from "./host";
import { githubActionAllowed, githubActionGrants } from "./githubGrants";
import { createSimPlatform } from "./platform";

for (const name of Object.keys(githubActionGrants) as GithubAction["action"][]) {
  it(`GitHub ${name} requires each declared grant`, () => {
    const action = { action: name } as GithubAction;
    expect(githubActionAllowed(allGrants, action)).toBe(true);
    for (const grant of githubActionGrants[name]) {
      expect(githubActionAllowed({ ...allGrants, [grant]: false }, action), grant).toBe(false);
    }
    const onlyRequired = Object.fromEntries(Object.keys(allGrants).map((grant) => [grant, false])) as Grants;
    for (const grant of githubActionGrants[name]) onlyRequired[grant] = true;
    expect(githubActionAllowed(onlyRequired, action)).toBe(true);
  });
}

it("requires approval for automatic merging but not leaving merging to the person", () => {
  const grants = { ...allGrants, approvals: false };
  expect(githubActionAllowed(grants, { action: "set_when_green", task_id: "task", when_green: "merge" })).toBe(false);
  expect(githubActionAllowed(grants, { action: "set_when_green", task_id: "task" })).toBe(true);
  expect(githubActionAllowed(allGrants, { action: "unknown" } as unknown as GithubAction)).toBe(false);
});

afterEach(() => { window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

it.each(["merge_pr", "merge_together"] as const)("the simulator transport refuses %s without approvals, then permits it to reach the host", async (action) => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const receipts: ServerFrame[] = [];
  control.listen((event) => { if (event.type === "frame" && event.frame.type === "receipt") receipts.push(event.frame); });
  const host = vi.spyOn(window.__sim!.host, "command");
  window.__sim!.permissions({ tasks: true, approvals: false });
  control.connect();
  await vi.advanceTimersByTimeAsync(900);
  const send = (id: string) => control.send({
    type: "command", command_id: id, issued_at: Date.now(), expected_revision: null, expires_at: null,
    body: { command: "github", action: { action, project_id: "missing", repo: "test/repo", number: 1, expected_head: "head", method: "squash", prs: [] } as GithubAction },
  });
  send("without-approval");
  await vi.advanceTimersByTimeAsync(900);
  expect(receipts).toMatchObject([{ type: "receipt", status: "rejected", error: { code: "permission_denied" } }]);
  expect(host).not.toHaveBeenCalled();
  window.__sim!.permissions({ approvals: true });
  control.connect();
  await vi.advanceTimersByTimeAsync(900);
  send("with-approval");
  await vi.advanceTimersByTimeAsync(900);
  expect(host).toHaveBeenCalledOnce();
  control.disconnect();
});
