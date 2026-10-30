// SPDX-License-Identifier: Apache-2.0
import type { Availability } from "../../protocol/types";
import { PROGRAM_NAME } from "../../brand";
import { Notice, availabilityText } from "../../design/Notice";
import { InlineCode } from "../../design/InlineCode";

export function Remedy({ text }: { text: string }) {
  const command = `${PROGRAM_NAME} desktop enable`;
  const at = text.indexOf(command);
  return at < 0 ? text : <>{text.slice(0, at)}<InlineCode className="cmd cmd--inline">{command}</InlineCode>{text.slice(at + command.length)}</>;
}
export function ReadinessNote({ what, availability }: { what: string; availability: Availability }) {
  const detail = availabilityText(availability);
  if (detail.ok) return null;
  return <Notice tone={availability.state === "unknown" ? "unknown" : "warn"} title={`${what}: ${detail.label.toLowerCase()}`}>
    {detail.detail && <Remedy text={detail.detail} />}
  </Notice>;
}
