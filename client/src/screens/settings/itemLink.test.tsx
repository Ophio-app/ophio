// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import * as fx from "../../dev/sim/scenario";
import { OBJECT_LINK_PREFIX, objectLink, objectLinkProblem, onObjectLink } from "../../platform/links";
import Settings from "./Settings";

vi.mock("../../app/context", () => ({
  useApp: () => ({ session: { query: vi.fn(), state: { set() {} } }, ui: { push() {} } }),
  useLink: () => ({ state: "offline" }),
}));
vi.mock("../../shell/Screen", () => ({ Screen: ({ children }: { children: unknown }) => children }));
vi.mock("./AgentsAndAccounts", () => ({ AgentsAndAccounts: () => null }));
vi.mock("./DataAndVoice", () => ({ DataAndVoice: () => null }));
vi.mock("./Notifications", () => ({ Notifications: () => null }));
vi.mock("../../app/github", () => ({ useGithubSupported: () => false }));
vi.mock("./AppUpdate", () => ({ useAppOffer: () => null }));
vi.mock("./Versions", () => ({ Versions: () => null }));
vi.mock("./Appearance", () => ({ Appearance: () => null }));
vi.mock("./Lock", () => ({ Lock: () => null }));
afterEach(cleanup);

const link = objectLink({ hostId: fx.ids.host, kind: "task", id: fx.ids.tFlaky });
function check(value: string) {
  fireEvent.change(screen.getByLabelText("Item link"), { target: { value } });
  fireEvent.click(screen.getByRole("button", { name: "Check link" }));
}

it("names what an item link starts with, and says when the rest is cut off", () => {
  expect(objectLinkProblem(link)).toBeNull();
  expect(objectLinkProblem(link.slice(OBJECT_LINK_PREFIX.length))).toContain(OBJECT_LINK_PREFIX);
  expect(objectLinkProblem(link.slice(0, -4))).toMatch(/incomplete/);
});
it("keeps a truncated paste in the field with the reason under it", () => {
  const accepted: string[] = [];
  const off = onObjectLink((url) => accepted.push(url));
  render(<Settings route={{ screen: "settings", section: "settings-link" }} />);
  const truncated = link.slice(0, -4);
  check(truncated);
  expect(accepted).toEqual([]);
  expect((screen.getByLabelText("Item link") as HTMLInputElement).value).toBe(truncated);
  expect(screen.getByRole("alert").textContent).toMatch(/incomplete/);
  // Fixing the text clears the reason; an accepted link empties the field.
  fireEvent.change(screen.getByLabelText("Item link"), { target: { value: link } });
  expect(screen.queryByRole("alert")).toBeNull();
  check(` ${link} `);
  expect(accepted).toEqual([link]);
  expect((screen.getByLabelText("Item link") as HTMLInputElement).value).toBe("");
  off();
});
