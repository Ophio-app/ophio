// SPDX-License-Identifier: Apache-2.0
import { Copy } from "lucide-react";
import { Button } from "./Button";
import { commandIn, remedyText } from "./remedy";
import { toast } from "./Toast";

/** A remedy in words, with any command for the computer shown on its own line
 * and a button to copy it, rather than typed out mid-sentence. */
export function Remedy({ text }: { text: string }) {
  const found = commandIn(text);
  if (!found) return <>{remedyText(text)}</>;
  const { lead, command } = found;
  return <span className="remedy">
    <span>{lead}</span>
    <code className="cmd">{command}</code>
    <Button size="sm" icon={Copy} onClick={() => void navigator.clipboard.writeText(command).then(
      () => toast("Command copied"),
      () => toast("Copy is unavailable. Select the command to copy it.", { tone: "stop" }),
    )}>Copy command</Button>
  </span>;
}
