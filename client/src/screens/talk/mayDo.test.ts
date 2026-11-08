// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import type { ConversationAccess, Project } from "../../protocol/types";
import { defaultAccess } from "./access";
import { governedByAccess, mayDo, type MayDoTask } from "./mayDo";

const project = (id: string, name: string, home = false) => ({ project_id: id, name, ...(home ? { home: true } : {}) }) as Project;
const ophio = project("p-ophio", "ophio");
const notes = project("p-notes", "notes");
const home = project("p-home", "Home", true);
const projects = { [ophio.project_id]: ophio, [notes.project_id]: notes, [home.project_id]: home };

const access = (a: Partial<ConversationAccess> = {}): ConversationAccess => ({ ...defaultAccess(ophio.project_id), ...a });
// A chat is created in Ask mode; only its access decides what it may do.
const chat = (a?: Partial<ConversationAccess>): MayDoTask => ({ project_id: ophio.project_id, conversation: true, mode: "ask", access: access(a) });
const task = (mode: MayDoTask["mode"], extra: Partial<MayDoTask> = {}): MayDoTask => ({ project_id: ophio.project_id, mode, ...extra });

describe("mayDo", () => {
  it.each<[string, MayDoTask, string]>([
    ["a new chat", chat(), "Chat, can read ophio and use the internet"],
    ["a chat without the internet", chat({ internet: false }), "Chat, can read ophio"],
    ["a chat that may change its project", chat({ write: [ophio.project_id] }), "Chat, can change ophio and use the internet"],
    ["a chat that may change its project, offline", chat({ write: [ophio.project_id], internet: false }), "Chat, can change ophio"],
    ["a chat that may change several projects", chat({ write: [ophio.project_id, notes.project_id], internet: false }), "Chat, can change ophio and notes"],
    ["several projects and the internet", chat({ write: [ophio.project_id, notes.project_id] }), "Chat, can change ophio and notes, and use the internet"],
    ["the home folder", chat({ write: [home.project_id], internet: false }), "Chat, can change your home folder and read ophio"],
    ["another project to read", chat({ read: [ophio.project_id, notes.project_id] }), "Chat, can read ophio and notes, and use the internet"],
    ["changing one project and reading another", chat({ read: [notes.project_id], write: [ophio.project_id] }), "Chat, can change ophio, read notes, and use the internet"],
    ["the whole computer to read", chat({ whole_read: true, internet: false }), "Chat, can read the whole computer"],
    ["the whole computer to read, changing one project", chat({ whole_read: true, write: [ophio.project_id], internet: false }), "Chat, can change ophio and read the whole computer"],
    ["the whole computer to change", chat({ whole_read: true, whole_write: true, internet: false }), "Chat, can use the whole computer"],
    ["the whole computer and the internet", chat({ whole_read: true, whole_write: true }), "Chat, can use the whole computer, use the internet, and reach your own network"],
    ["the person's own network", chat({ local_network: true }), "Chat, can read ophio, use the internet, and reach your own network"],
    ["the own network without the internet means nothing", chat({ local_network: true, internet: false }), "Chat, can read ophio"],
    ["a project this computer no longer lists", chat({ read: ["p-gone"], write: ["p-gone"], internet: false }), "Chat, can read ophio"],
    ["a Plan task", task("plan"), "Task in Plan mode, changes nothing"],
    ["an Ask task", task("ask"), "Task in Ask mode, changes nothing"],
    ["a Work task", task("work"), "Task in Work mode, can change ophio"],
    ["a task from before modes", task(undefined), "Task in Work mode, can change ophio"],
    ["a Work task in the home folder", { project_id: home.project_id, mode: "work" }, "Task in Work mode, can change your home folder"],
    // The computer keeps a chat turned into a task with its own project to change.
    ["a chat turned into a task", task("work", { conversation: false, access: access({ write: [ophio.project_id] }) }), "Task in Work mode, can change ophio and use the internet"],
    ["a chat turned into a task, reading more", task("work", { access: access({ read: [notes.project_id], write: [ophio.project_id], internet: false }) }), "Task in Work mode, can change ophio and read notes"],
    ["a chat turned into a task with the whole computer", task("work", { access: access({ whole_write: true, internet: false }) }), "Task in Work mode, can use the whole computer"],
    // An outsider's task, held to reading its project, says so.
    ["an outsider's Plan task held read-only", task("plan", { access: access({ internet: false }) }), "Task in Plan mode, can read ophio"],
    ["a held Work task that may change nothing", task("work", { access: access({ internet: false }) }), "Task in Work mode, can read ophio"],
  ])("%s", (_, t, line) => {
    expect(mayDo(t, true, projects)).toBe(line);
  });

  it("reads a chat as the computer holds it where it can't hold it to a choice", () => {
    const unchosen: MayDoTask = { project_id: ophio.project_id, conversation: true, mode: "ask" };
    expect(mayDo(unchosen, false, projects)).toBe("Chat, can read ophio");
    expect(mayDo(unchosen, true, projects)).toBe("Chat, can read ophio and use the internet");
    // What was chosen is what it says, held or not.
    expect(mayDo(chat({ write: [ophio.project_id] }), false, projects)).toBe("Chat, can change ophio and use the internet");
  });

  it("never calls a chat Ask", () => {
    for (const a of [{}, { internet: false }, { write: [ophio.project_id] }, { whole_write: true }]) {
      expect(mayDo(chat(a), true, projects)).not.toMatch(/\bAsk\b/);
      expect(mayDo(chat(a), false, projects)).not.toMatch(/\bAsk\b/);
    }
  });

  it("names a project it can't find as its project", () => {
    expect(mayDo({ project_id: "p-gone", mode: "work" }, true, projects)).toBe("Task in Work mode, can change its project");
    expect(mayDo({ project_id: "p-gone", conversation: true }, false, projects)).toBe("Chat, can read its project");
  });

  it("knows what its access governs", () => {
    expect(governedByAccess({ conversation: true })).toBe(true);
    expect(governedByAccess({ conversation: false, access: access() })).toBe(true);
    expect(governedByAccess({ conversation: false })).toBe(false);
  });
});
