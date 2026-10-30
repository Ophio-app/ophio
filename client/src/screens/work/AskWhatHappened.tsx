// SPDX-License-Identifier: Apache-2.0
//
// Summarize from a receipt, where it says something is unsure: asks the
// task's own agent, read-only, what happened, and follows its answer in
// Chat. Chat's message box offers the same.

import { useState } from "react";
import { ScrollText } from "lucide-react";
import { useApp, useHost, useUi } from "../../app/context";
import { draftKey } from "../../app/ui";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import type { Run, Task } from "../../protocol/types";
import { conversationSummary } from "../talk/conversation";
import { send } from "../talk/send";
import { READ_ONLY_TURN_FEATURE, summarizable, summaryRequest } from "../talk/summarize";
import { useCanWork } from "./shared";

/** Whether this receipt can ask the task's agent what happened. */
export function useSummarizable(task: Task, run: Run | null): boolean {
  const harness = useHost((s) => s.harnesses.find((h) => h.kind === task.harness) ?? null);
  const supported = useHost((s) => !!s.capabilities?.features.includes(READ_ONLY_TURN_FEATURE));
  return supported && summarizable(task, run, harness);
}

export function SummarizeButton({ task, onSent }: { task: Task; onSent: () => void }) {
  const app = useApp();
  const can = useCanWork();
  // A message not yet known to have arrived is settled first, in Chat.
  const inDoubt = useUi((s) => {
    const phase = s.drafts[draftKey(task.project_id, task.task_id)]?.attempt?.phase;
    return phase === "sending" || phase === "unknown";
  });
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ask = async () => {
    setSending(true); setError(null);
    const out = await send(app, task.conversation ? conversationSummary(task.project_id, task) : summaryRequest(task.project_id, task), () => {});
    setSending(false);
    if (out.ok) onSent();
    else setError(out.attempt.detail ?? "It wasn’t sent.");
  };
  return <>
    <Button icon={ScrollText} pending={sending} disabled={!can || sending || inDoubt} onClick={() => void ask()}>Summarize what happened</Button>
    {error && <Notice tone="stop">{error}</Notice>}
  </>;
}
