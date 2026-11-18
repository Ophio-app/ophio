// SPDX-License-Identifier: Apache-2.0
import type { Approval, TimelineItem } from "../../protocol/types";

export function answeredQuestion(a: Approval): boolean {
  return a.kind === "question" && (a.state === "allowed" || a.state === "resolved_elsewhere");
}

/** No journal position for the answer is exposed by the protocol. Require a
 * successful answer hand-off and strictly later host times, as well as the
 * question's journal anchor. Equal times cannot prove which came first. */
export function answerTime(a: Approval): number | null {
  const delivery = a.delivery;
  if (!answeredQuestion(a) || !delivery || delivery.error || delivery.decision.decision !== "answer"
    || a.outcome === "denied" || a.resolved_at === null) return null;
  const at = Math.max(delivery.at, a.resolved_at);
  return Number.isFinite(at) && at >= a.requested_at ? at : null;
}

export function questionReply(a: Approval, items: TimelineItem[], approvals: Record<string, Approval>) {
  const at = answerTime(a);
  const anchor = items.findIndex(i => i.run_id === a.run_id && i.item.kind === "approval" && i.item.approval_id === a.approval_id);
  const messages: TimelineItem[] = [];
  let closed = false;
  let uncertain = false;
  if (at === null || anchor < 0 || items[anchor]!.at > at) return { messages, anchored: false, closed, uncertain };

  // A message that began before the answer must not lend its later segments
  // to it. Use durable identities where available, never text/keyword guesses.
  const excluded = new Set(items.filter(i => i.run_id === a.run_id && (i.seq <= items[anchor]!.seq || i.at <= at))
    .flatMap(i => i.item.kind === "agent_message" && i.item.message_key ? [i.item.message_key] : []));
  const seen = new Set<string>();
  for (const i of items.slice(anchor + 1)) {
    const body = i.item;
    if (body.kind === "user_message" || body.kind === "run_started" || body.kind === "run_ended") {
      closed = true;
      break;
    }
    if (body.kind === "approval") {
      const next = approvals[body.approval_id];
      // An unavailable request might be the next question. Stop, rather than
      // attaching its answer here. Ordinary permission cards do not end a reply.
      if (!next || next.kind === "question" || next.run_id !== a.run_id) {
        closed = true;
        uncertain ||= !next;
        break;
      }
    }
    if (i.run_id !== a.run_id || body.kind !== "agent_message") continue;
    if (i.at <= at) {
      uncertain ||= i.at === at && !!body.text.trim();
      continue;
    }
    if (body.message_key && excluded.has(body.message_key)) { uncertain = true; continue; }
    // The first loaded segment must start a message, not a pre-answer tail.
    if (body.message_key && !seen.has(body.message_key) && (body.text_offset ?? 0) > 0) {
      excluded.add(body.message_key);
      uncertain = true;
      continue;
    }
    if (body.message_key) seen.add(body.message_key);
    if (body.text.trim()) messages.push(i);
  }
  return { messages, anchored: true, closed, uncertain };
}
