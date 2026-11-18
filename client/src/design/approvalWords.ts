// SPDX-License-Identifier: Apache-2.0
import type { Approval } from "../protocol/types";

/**
 * The word for saying no to `approval`: a request is rejected, a question is
 * declined (nobody rejects a question), and an agent's request for more
 * access is answered "Not now".
 */
export function refusalWord(approval: Pick<Approval, "kind" | "access_request">): "Reject" | "Decline" | "Not now" {
  if (approval.access_request) return "Not now";
  return approval.kind === "question" ? "Decline" : "Reject";
}
