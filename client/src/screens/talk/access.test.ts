// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { Project } from "../../protocol/types";
import { accessHeld, askAccess, defaultAccess, describeAccess } from "./access";

const project = (id: string, name: string, home = false) => ({ project_id: id, name, ...(home ? { home: true } : {}) }) as Project;
const tide = project("p-tide", "tidewater-api");
const site = project("p-site", "website");
const home = project("p-home", "Home", true);
const projects = { [tide.project_id]: tide, [site.project_id]: site, [home.project_id]: home };

it("says what a new conversation's agent may reach, adapting to each choice", () => {
  expect(describeAccess(askAccess(tide.project_id), tide, projects))
    .toBe("The agent can read tidewater-api but can’t change anything. It can’t reach the internet. Turn it into a task when you want something done.");
  expect(describeAccess(defaultAccess(tide.project_id), tide, projects))
    .toBe("The agent can read tidewater-api but can’t change anything. It can look things up on the internet. Turn it into a task when you want something done.");
  const writes = { ...defaultAccess(tide.project_id), read: [tide.project_id, site.project_id, home.project_id], write: [tide.project_id] };
  expect(describeAccess(writes, tide, projects))
    .toBe("The agent can change tidewater-api and run commands there, and read website and your home folder. It can look things up on the internet.");
  expect(describeAccess({ ...writes, whole_read: true }, tide, projects))
    .toBe("The agent can read anything on this computer, and change tidewater-api and run commands there. It can look things up on the internet.");
  expect(describeAccess({ ...askAccess(tide.project_id), whole_read: true }, tide, projects))
    .toBe("The agent can read anything on this computer but can’t change anything. It can’t reach the internet. Turn it into a task when you want something done.");
  expect(describeAccess({ ...defaultAccess(tide.project_id), whole_read: true, whole_write: true }, tide, projects))
    .toBe("The agent can read and change anything on this computer, and run any command. It can look things up on the internet and reach your own network, such as your tailnet.");
  expect(describeAccess({ ...defaultAccess(tide.project_id), local_network: true }, tide, projects))
    .toMatch(/It can look things up on the internet and reach your own network, such as your tailnet\./);
  expect(describeAccess({ ...askAccess(tide.project_id), local_network: true }, tide, projects))
    .toMatch(/It can’t reach the internet\./);
});

it("names Home as the home folder and leaves out projects the computer no longer lists", () => {
  expect(describeAccess({ ...defaultAccess(home.project_id), read: [home.project_id, "p-gone"] }, home, projects))
    .toMatch(/^The agent can read your home folder but can’t change anything\./);
});

it("holds a conversation to a choice only once the computer says it can", () => {
  expect(accessHeld(null)).toBe(false);
  expect(accessHeld({ conversation_access: { state: "unavailable", reason: "x" } } as never)).toBe(false);
  expect(accessHeld({ conversation_access: { state: "available" } } as never)).toBe(true);
});
