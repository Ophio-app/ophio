// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it } from "vitest";
import { focusAfterDecision, OPEN_DECISION } from "./decisionFocus";

afterEach(() => { document.body.replaceChildren(); });

function screenWith(...ids: string[]) {
  const route = document.createElement("div");
  route.className = "route";
  route.innerHTML = ids.map((id) => `<article id="${id}" ${OPEN_DECISION}=""></article>`).join("") + `<h2 id="title">Waiting for you</h2>`;
  document.body.append(route);
  return (id: string) => document.getElementById(id)!;
}

it("moves to the next request still open below the decided one", () => {
  const el = screenWith("a", "b", "c");
  focusAfterDecision(el("b"))();
  expect(document.activeElement).toBe(el("c"));
  expect(el("c").tabIndex).toBe(-1);
});

it("wraps to the first open request when the decided one was last", () => {
  const el = screenWith("a", "b");
  focusAfterDecision(el("b"))();
  expect(document.activeElement).toBe(el("a"));
});

it("falls back when nothing else waits, and stays inside the decided request's screen or sheet", () => {
  const el = screenWith("a");
  const sheet = document.createElement("dialog");
  sheet.innerHTML = `<article id="other" ${OPEN_DECISION}=""></article>`;
  document.body.append(sheet);
  focusAfterDecision(el("a"), el("title"))();
  expect(document.activeElement).toBe(el("title"));
  el("title").blur();
  focusAfterDecision(el("a"))();
  expect(document.activeElement).toBe(el("a"));
});

it("does nothing without the decided element", () => {
  const el = screenWith("a");
  el("a").tabIndex = -1;
  el("a").focus();
  focusAfterDecision(null, el("title"))();
  expect(document.activeElement).toBe(el("a"));
});

it("still finds the next request when the decided one was redrawn meanwhile", () => {
  const el = screenWith("a", "b");
  const refocus = focusAfterDecision(el("a"), el("title"));
  el("a").remove();
  refocus();
  expect(document.activeElement).toBe(el("b"));
  const last = focusAfterDecision(el("b"), el("title"));
  el("b").remove();
  last();
  expect(document.activeElement).toBe(el("title"));
});

it("leaves focus the person moved elsewhere while the decision was on its way", () => {
  const el = screenWith("a", "b");
  const elsewhere = document.body.appendChild(document.createElement("button"));
  const refocus = focusAfterDecision(el("a"));
  elsewhere.focus();
  refocus();
  expect(document.activeElement).toBe(elsewhere);
});
