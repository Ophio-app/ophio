// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { approvalCases as cases, approvals } from "../dev/sim/scenario";
import { ApprovalCard, ApprovalDetailText } from "./ApprovalCard";
import { clockTime } from "./time";
afterEach(cleanup);
it("disables both offline decisions with the same connection explanation", () => {
  const onDecide = vi.fn();
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId="phone" canDecide
    disabled disabledReason="Connect to the computer to answer this request." onDecide={onDecide} />);
  for (const name of ["Deny…", "Allow"]) {
    const button = screen.getByRole("button", { name });
    expect(button).toHaveProperty("disabled", true);
    fireEvent.click(button);
  }
  expect(screen.getByText("Connect to the computer to answer this request.")).toBeTruthy();
  expect(screen.queryByLabelText("Reason (optional)")).toBeNull();
  expect(onDecide).not.toHaveBeenCalled();
});

it("drops the unconfirmed-decision warning once reconnecting lets the request be answered again", async () => {
  const unconfirmed = "The computer hasn’t confirmed your decision. Reconnect to reconcile it.";
  const onDecide = vi.fn().mockRejectedValueOnce({ error: { code: "outcome_unknown", message: unconfirmed } });
  const approval = approvals(Date.now())[0]!;
  const card = (disabled: boolean) => <ApprovalCard approval={approval} agent="Claude Code" myDeviceId="phone" canDecide
    disabled={disabled} disabledReason={unconfirmed} onDecide={onDecide} />;
  const { rerender } = render(card(false));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Allow" })); });
  rerender(card(true));
  // Said once, as the alert, while the outcome is still being learned.
  expect(screen.getAllByText(unconfirmed)).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe(unconfirmed);
  rerender(card(false));
  expect(screen.queryByText(unconfirmed)).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByRole("button", { name: "Allow" })).toHaveProperty("disabled", false);
});

it("makes denial primary in the review form and sends the reason", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={approvals(Date.now())[0]!} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />);
  fireEvent.click(screen.getByRole("button", { name: "Deny…" }));
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  const deny = screen.getByRole("button", { name: /^Deny$/ });
  expect(deny.classList.contains("btn--primary")).toBe(true);
  fireEvent.change(screen.getByLabelText("Reason (optional)"), { target: { value: "Wait for the maintainer" } });
  await act(async () => { fireEvent.click(deny); });
  expect(onDecide).toHaveBeenCalledWith({ decision: "deny", reason: "Wait for the maintainer" });
});

import type { Approval } from "../protocol/types";
it.each(cases.filter(c => !("questions" in c.approval)))("shows $tool facts without opening Exact request, and says when they were shortened", ({ approval, input }) => {
  render(<ApprovalCard approval={{ ...approvals(Date.now())[0]!, ...approval, kind: approval.kind as Approval["kind"], raw_request: JSON.stringify(input) }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(screen.getByRole("heading", { name: approval.title })).toBeTruthy();
  const detail = document.querySelector(".approval__detail");
  expect(detailLines(detail!)).toEqual(approval.detail_lines!.map(line => ({ text: line.label ? `${line.label}: ${line.text}` : line.text, code: line.code && line.change !== "omitted" ? line.text : null, change: line.change ?? null })));
  expect(detailLines(detail!).map(line => line.change ? `${PLAIN_MARK[line.change]} ${line.text}` : line.text).join("\n")).toBe(approval.detail);
  expect(detail?.closest("details")).toBeNull();
  expect(document.querySelector("details")?.open).toBe(false);
  expect(screen.getByText("This request once")).toBeTruthy();
  expect(screen.queryByText("Shortened · the full request is under Exact request") !== null).toBe(approval.shortened);
});

/** How the companion's plain detail marks each line of a proposed change. */
const PLAIN_MARK: Record<string, string> = { unchanged: " ", removed: "-", added: "+", omitted: "⋯" };

/**
 * Each shown line, the part of it set as code, and for a line of a proposed
 * change how it changes, read from what is seen rather than what is said.
 */
function detailLines(root: Element) {
  return [...root.querySelectorAll(".approval__line:not(.approval__change), .approval__change-line")].map(line => {
    const change = [...line.classList].find(name => name.startsWith("approval__change-line--"))?.slice("approval__change-line--".length) ?? null;
    const seen = line.cloneNode(true) as Element;
    seen.querySelectorAll(".visually-hidden, .approval__change-mark").forEach(hidden => hidden.remove());
    const code = seen.matches("code") ? seen : seen.querySelector("code");
    return { text: seen.textContent, code: code?.textContent ?? null, change };
  });
}

const change = cases.find(c => c.input && (c.input as { file_path?: string }).file_path === "client/src/screens/Home.tsx")!;

it("shows an edit's proposed change as marked lines, said as removed and added, and leaves it out of a brief row", () => {
  const approval: Approval = { ...approvals(Date.now())[0]!, ...change.approval, kind: "file_edit", raw_request: JSON.stringify(change.input) };
  const { unmount } = render(<ApprovalCard approval={approval} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const group = screen.getByRole("group", { name: "Proposed change" });
  const lines = [...group.querySelectorAll(".approval__change-line")];
  const marks = lines.map(line => line.querySelector(".approval__change-mark")!);
  expect(marks.every(mark => mark.getAttribute("aria-hidden") === "true")).toBe(true);
  const said = (kind: string) => lines.filter(line => line.classList.contains(`approval__change-line--${kind}`));
  expect(said("removed").length).toBeGreaterThan(0);
  expect(said("added").length).toBeGreaterThan(0);
  for (const line of said("removed")) {
    expect(line.querySelector(".approval__change-mark")!.textContent).toBe("−");
    expect(line.querySelector("code .visually-hidden")!.textContent).toBe("Removed: ");
  }
  for (const line of said("added")) {
    expect(line.querySelector(".approval__change-mark")!.textContent).toBe("+");
    expect(line.querySelector("code .visually-hidden")!.textContent).toBe("Added: ");
  }
  for (const line of said("unchanged")) expect(line.querySelector(".visually-hidden")).toBeNull();
  expect(group.textContent).toContain("Removed:   if (!tasks.length) return <Empty />;");
  expect(group.textContent).toContain('Added:   if (!tasks.length) return <Empty title="Nothing waiting" />;');
  unmount();

  render(<p><ApprovalDetailText approval={approval} brief /></p>);
  expect(document.querySelector(".approval__change")).toBeNull();
  expect(document.body.textContent).toContain("client/src/screens/Home.tsx");
});

it("keeps a command's own line breaks and spacing, and leaves a detail without code as prose", () => {
  const approval = approvals(Date.now())[0]!;
  const { unmount } = render(<ApprovalCard approval={{ ...approval, detail: "cd crates\n  cargo test\nRun the tests", detail_lines: [{ text: "cd crates\n  cargo test", code: true }, { text: "Run the tests" }] }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  expect(detailLines(document.querySelector(".approval__detail")!)).toEqual([{ text: "cd crates\n  cargo test", code: "cd crates\n  cargo test", change: null }, { text: "Run the tests", code: null, change: null }]);
  unmount();
  render(<ApprovalCard approval={{ ...approval, kind: "other", detail: "Needs the network to fetch crates.", detail_lines: [] }} agent="Codex" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const detail = document.querySelector(".approval__detail")!;
  expect(detail.textContent).toBe("Needs the network to fetch crates.");
  expect(detail.querySelector("code, .approval__line")).toBeNull();
});

const asked = cases.find(c => c.tool === "AskUserQuestion")!;
const question = (): Approval => ({ ...approvals(Date.now())[0]!, ...asked.approval, kind: "question", raw_request: JSON.stringify(asked.input) });

it("shows each question with its options and what they mean, and sends the chosen answers", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={onDecide} />);
  expect(screen.getByRole("heading", { name: "Answer 2 questions" })).toBeTruthy();
  expect(document.querySelector(".approval__detail")).toBeNull();
  expect(screen.getByText("Only your answer; nothing runs")).toBeTruthy();
  const [rounding, reports] = document.querySelectorAll("fieldset");
  expect(rounding!.querySelector("legend")!.textContent).toBe("RoundingHow should totals be rounded?");
  expect(reports!.querySelector("legend")!.textContent).toBe("ReportsWhich reports need the change?Choose one or more");
  const nearest = screen.getByRole("radio", { name: "Nearest cent" });
  expect(document.getElementById(nearest.getAttribute("aria-describedby")!)!.textContent).toBe("Round half up to two decimal places");
  expect(screen.getAllByRole("radio").map(r => r.getAttribute("name"))).toEqual([nearest.getAttribute("name"), nearest.getAttribute("name")]);
  expect(screen.getAllByRole("checkbox").map(c => c.closest("label")!.textContent)).toEqual(["Invoices", "Receipts", "StatementsMonthly account statements"]);
  expect(screen.getByRole("checkbox", { name: "Invoices" }).getAttribute("aria-describedby")).toBeNull();
  const send = screen.getByRole("button", { name: "Send answer" });
  expect(send).toHaveProperty("disabled", true);

  fireEvent.click(screen.getByRole("radio", { name: "Always up" }));
  expect(send).toHaveProperty("disabled", true);
  fireEvent.click(screen.getByRole("checkbox", { name: "Statements" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Invoices" }));
  fireEvent.change(screen.getByLabelText("Add your own answer"), { target: { value: " Only paid ones " } });
  expect(send).toHaveProperty("disabled", false);
  await act(async () => { fireEvent.click(send); });
  expect(onDecide).toHaveBeenCalledWith({
    decision: "answer",
    text: "Rounding: Always up; Reports: Invoices, Statements, Only paid ones",
    answers: [
      { question: "How should totals be rounded?", choices: ["Always up"] },
      { question: "Which reports need the change?", choices: ["Invoices", "Statements", "Only paid ones"] },
    ],
  });
});

it("takes one option or the person's own words for a single-choice question, never both", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const one = { ...question(), title: "How should totals be rounded?", questions: question().questions!.slice(0, 1) };
  render(<ApprovalCard approval={one} agent="Claude Code" myDeviceId={null} canDecide onDecide={onDecide} />);
  expect(screen.getByRole("heading", { name: "How should totals be rounded?" })).toBeTruthy();
  expect(document.querySelector("fieldset .visually-hidden")!.textContent).toBe("How should totals be rounded?");
  const nearest = screen.getByRole("radio", { name: "Nearest cent" });
  const own = screen.getByLabelText("Or answer in your own words");
  fireEvent.click(nearest);
  fireEvent.change(own, { target: { value: "Nearest, but never below cost" } });
  expect(nearest).toHaveProperty("checked", false);
  fireEvent.click(nearest);
  expect(own).toHaveProperty("value", "");
  fireEvent.change(own, { target: { value: "Nearest, but never below cost" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send answer" })); });
  expect(onDecide).toHaveBeenCalledWith({
    decision: "answer",
    text: "Nearest, but never below cost",
    answers: [{ question: "How should totals be rounded?", choices: ["Nearest, but never below cost"] }],
  });
});

it("asks a secret hidden and never reads the answer back", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const secret = { ...question(), title: "What's the staging password?", questions: [{ question: "What's the staging password?", options: [], secret: true }] };
  const { unmount } = render(<ApprovalCard approval={secret} agent="Codex" myDeviceId={null} canDecide onDecide={onDecide} />);
  const typed = screen.getByLabelText("Type it");
  expect(typed.getAttribute("type")).toBe("password");
  fireEvent.change(typed, { target: { value: "hunter2" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Send answer" })); });
  const answers = [{ question: "What's the staging password?", choices: ["hunter2"] }];
  expect(onDecide).toHaveBeenCalledWith({ decision: "answer", text: "hunter2", answers });
  unmount();
  render(<ApprovalCard approval={{ ...secret, state: "allowed", resolved_by: { kind: "device", device_id: "phone" }, resolved_at: Date.now(),
    delivery: { decision: { decision: "answer", text: "hunter2", answers }, at: Date.now(), error: null } }}
  agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.body.textContent).not.toContain("hunter2");
  expect([...document.querySelectorAll(".approval__settled-what")].map(p => p.textContent)).toContain("Answered");
});

it("declines a question without sending a reason it never asked for", async () => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={onDecide} />);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Decline" })); });
  expect(onDecide).toHaveBeenCalledWith({ decision: "deny", reason: null });
});

it("settles as Answered with the answer given, not Allowed", () => {
  const answers = [
    { question: "How should totals be rounded?", choices: ["Always up"] },
    { question: "Which reports need the change?", choices: ["Invoices", "Statements"] },
  ];
  render(<ApprovalCard approval={{ ...question(), state: "allowed", resolved_by: { kind: "device", device_id: "phone" }, resolved_at: Date.now(),
    delivery: { decision: { decision: "answer", text: "Rounding: Always up; Reports: Invoices, Statements", answers }, at: Date.now(), error: null } }}
  agent="Claude Code" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(document.querySelector(".approval__settled-title")!.textContent).toMatch(/^Answered · /);
  expect(screen.getByText(/^Answered by you\. Sent to the agent/)).toBeTruthy();
  expect([...document.querySelectorAll(".approval__settled-what")].map(p => p.textContent)).toEqual([
    "Answer 2 questions", "Rounding: Always up", "Reports: Invoices, Statements",
  ]);
  expect(screen.queryByRole("radio")).toBeNull();
  expect(document.querySelector(".approval__detail")).toBeNull();
});

it("makes Send answer the primary on a question, saying what it waits for, and Decline secondary", () => {
  const { unmount } = render(<ApprovalCard approval={question()} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const send = screen.getByRole("button", { name: "Send answer" });
  const decline = screen.getByRole("button", { name: "Decline" });
  expect(send.classList.contains("btn--primary")).toBe(true);
  expect(decline.classList.contains("btn--secondary")).toBe(true);
  expect(send).toHaveProperty("disabled", true);
  expect(document.getElementById(send.getAttribute("aria-describedby")!)!.textContent).toBe("Answer each question to send.");
  fireEvent.click(screen.getByRole("radio", { name: "Always up" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Invoices" }));
  expect(send).toHaveProperty("disabled", false);
  expect(send.getAttribute("aria-describedby")).toBeNull();
  expect(screen.queryByText("Answer each question to send.")).toBeNull();
  unmount();
  // Asked in words only: the answer is typed.
  render(<ApprovalCard approval={{ ...question(), questions: [], detail: "Which branch should I use?" }} agent="Codex" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const typed = screen.getByRole("button", { name: "Send answer" });
  expect(document.getElementById(typed.getAttribute("aria-describedby")!)!.textContent).toBe("Write an answer to send it.");
  fireEvent.change(screen.getByLabelText("Your answer"), { target: { value: "main" } });
  expect(typed).toHaveProperty("disabled", false);
});

it("keeps focus on the answered request, or moves it to the next one waiting on the same screen", async () => {
  const first = approvals(Date.now())[0]!;
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const { unmount } = render(<ApprovalCard approval={first} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />);
  const allow = screen.getByRole("button", { name: "Allow" });
  allow.focus();
  await act(async () => { fireEvent.click(allow); });
  expect(document.activeElement).toBe(document.querySelector("article.approval"));
  unmount();

  render(<div className="route">
    <ApprovalCard approval={first} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />
    <ApprovalCard approval={{ ...first, approval_id: `${first.approval_id}-next` }} agent="Claude Code" myDeviceId="phone" canDecide onDecide={onDecide} />
  </div>);
  const [card, next] = [...document.querySelectorAll<HTMLElement>("article.approval")];
  await act(async () => { fireEvent.click(within(card!).getByRole("button", { name: "Allow" })); });
  expect(document.activeElement).toBe(next);
  expect(card!.hasAttribute("data-open-decision")).toBe(false);
});

it("gives a screen reader the expiry as one value, not a clock time and parentheses read apart", () => {
  const expires = Date.now() + 8 * 60_000 + 30_000;
  render(<ApprovalCard approval={{ ...approvals(Date.now())[0]!, expires_at: expires }} agent="Claude Code" myDeviceId={null} canDecide onDecide={async () => {}} />);
  const value = screen.getByText("Expires").nextElementSibling!;
  expect(value.childNodes).toHaveLength(1);
  const time = value.firstElementChild!;
  expect(time.tagName).toBe("TIME");
  expect(time.textContent).toBe(`${clockTime(expires)} (in 8 min)`);
  expect(time.getAttribute("aria-label")).toBe(`${clockTime(expires)}, in 8 minutes`);
  expect(time.getAttribute("dateTime")).toBe(new Date(expires).toISOString());
});

// These same fixtures are checked against the native adapter's mapped event
// and response, so a phone answer must satisfy both sides of the contract.
import codexQuestions from "../../../crates/harness-codex/test-fixtures/questions.json";
it.each(codexQuestions)("answers Codex $name questions through the mapped approval", async (fixture) => {
  const onDecide = vi.fn().mockResolvedValue(undefined);
  const approval: Approval = { ...approvals(Date.now())[0]!, ...fixture.approval, kind: "question", raw_request: JSON.stringify(fixture.input) };
  const { unmount } = render(<ApprovalCard approval={approval} agent="Codex" myDeviceId="phone" canDecide onDecide={onDecide} />);
  expect(screen.getByRole("heading", { name: fixture.approval.title })).toBeTruthy();
  const send = screen.getByRole("button", { name: "Send answer" });
  expect(send).toHaveProperty("disabled", true);
  const fields = document.querySelectorAll("fieldset");
  for (const [i, q] of fixture.approval.questions.entries()) {
    expect(fields[i]!.querySelector("legend")!.textContent).toContain(q.question);
    const choice = fixture.decision.answers[i]!.choices[0]!;
    if (q.options.some(o => o.label === choice)) {
      fireEvent.click(screen.getByRole("radio", { name: choice }));
    } else {
      fireEvent.change(fields[i]!.querySelector("textarea")!, { target: { value: choice } });
    }
    expect(send).toHaveProperty("disabled", i < fields.length - 1);
  }
  await act(async () => { fireEvent.click(send); });
  expect(onDecide).toHaveBeenCalledWith(fixture.decision);
  unmount();
  render(<ApprovalCard approval={{ ...approval, state: "allowed", resolved_at: Date.now(), resolved_by: { kind: "device", device_id: "phone" },
    delivery: { decision: { ...fixture.decision, decision: "answer" }, at: Date.now(), error: null } }}
    agent="Codex" myDeviceId="phone" canDecide onDecide={onDecide} />);
  for (const [i, answer] of fixture.decision.answers.entries()) {
    const prefix = fixture.decision.answers.length > 1 ? `${fixture.approval.questions[i]!.header}: ` : "Your answer: ";
    expect(screen.getByText(prefix + answer.choices.join(", "))).toBeTruthy();
  }
});

it("keeps answers with repeated wording under their own headers and hides the secret one", () => {
  const questions = [
    { id: "public", header: "Name", question: "Which value?", options: [] },
    { id: "secret", header: "Password", question: "Which value?", options: [], secret: true },
  ];
  const answers = [
    { id: "secret", question: "Which value?", choices: ["private-value"] },
    { id: "public", question: "Which value?", choices: ["public-name"] },
  ];
  render(<ApprovalCard approval={{ ...question(), questions, state: "allowed", resolved_at: Date.now(),
    resolved_by: { kind: "device", device_id: "phone" },
    delivery: { decision: { decision: "answer", text: "public-name; private-value", answers }, at: Date.now(), error: null } }}
    agent="Codex" myDeviceId="phone" canDecide onDecide={async () => {}} />);
  expect(screen.getByText("Name: public-name")).toBeTruthy();
  expect(screen.getByText("Password: answered")).toBeTruthy();
  expect(document.body.textContent).not.toContain("private-value");
});
