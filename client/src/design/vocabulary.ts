// SPDX-License-Identifier: Apache-2.0
//
// The words Ophio says to the person. Each means one thing everywhere, and
// copy says what happened to their work, not what the computer did with a
// message: "Denied. The agent carries on without it.", never "The computer
// accepted your decision." vocabulary.test.ts reads every text a person can
// see and fails on a banned word. Messages the computer sends for the phone
// to show (errors, receipts, readiness) use the same words, unchecked.

/** A term, and the one thing it means. */
export const GLOSSARY: Record<string, string> = {
  "the computer": "The person's own computer, where the agents work. Its name where the copy can say it; never the host, companion or daemon.",
  "this device": "The phone or tablet in the person's hand, as Settings groups it.",
  agent: "Claude, Codex or another coding agent doing a task.",
  task: "One piece of work an agent does. Tasks lists them; a chat can carry on into one.",
  project: "A folder on the computer that agents work in. Home is the default.",
  "Needs you": "Tasks waiting on the person: a decision, an answer or a check.",
  "Next up": "Tasks that start by themselves, in turn.",
  "Waiting to start": "A task the computer has that hasn't started. Plain \"Waiting\" is a task waiting on an answer.",
  Sent: "On the computer. \"Sending\" comes before it, \"Delivered\" once the agent has it.",
  Stop: "Ends what an agent is doing, at most two taps away. \"Stop sent\" until it has stopped.",
  "Agent paused": "An agent held off the screen until the person picks Resume agent.",
  "Unread, Read": "Finished tasks, before and after the person has looked.",
  permission: "What a device may do, by the names Settings gives them (Terminals, Manage access). Never a grant.",
  "Agent sign-in": "The account an agent itself signs in with. Agent logins are the websites it may sign in to for a task.",
};

/** Machine words that have reached copy before, with what to say instead. */
export const BANNED: { word: RegExp; instead: string }[] = [
  { word: /\baccepted\b/i, instead: "what happened to the work: Sent, Allowed, Stop sent, Waiting to start" },
  { word: /\b(host|companion|daemon)\b/i, instead: "the computer, or its name" },
  { word: /\blease\b/i, instead: "control" },
  { word: /\bgrants?\b/i, instead: "permission, by its name in Settings" },
  { word: /\backnowledg/i, instead: "what the person is waiting for, such as the agent stopping" },
];
