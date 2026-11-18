// SPDX-License-Identifier: Apache-2.0
import type { ModelOption } from "../../protocol/types";

/** A long native catalog, including the raw labels older computers send for exact versions. */
export const claudeModels: ModelOption[] = [
  ["opus", "Claude Opus 5.5"],
  ["fable", "Claude Fable 5.2"],
  ["sonnet", "Claude Sonnet 5.5"],
  ["haiku", "Claude Haiku 5.5"],
  ["fable5.1", "Claude Fable 5.1"],
  ["haiku4.5", "Claude Haiku 4.5"],
  ["claude-sonnet-5", "Claude Sonnet 5"],
  ["opus5", "Claude Opus 5"],
  ["fable5", "Claude Fable 5"],
  ["opus4.8", "Claude Opus 4.8"],
  ["opus4.7", "Claude Opus 4.7"],
  ["opus4.6", "Claude Opus 4.6"],
  ["sonnet4.6", "Claude Sonnet 4.6"],
  ["claude-opus-5-5", "claude-opus-5-5"],
  ["claude-fable-5-2", "claude-fable-5-2"],
  ["claude-sonnet-5-5", "claude-sonnet-5-5"],
  ["claude-haiku-5-5", "claude-haiku-5-5"],
].map(([id, label]) => ({
  id: id!, label: label!, is_default: id === "opus", suggested_for_computer_use: id === "opus",
  details: { alias: !id!.startsWith("claude-"), default_effort: "high", efforts: [
    { id: "low", label: "Low" }, { id: "medium", label: "Medium" },
    { id: "high", label: "High" }, { id: "xhigh", label: "Extra high" }, { id: "max", label: "Maximum" },
  ] },
}));
