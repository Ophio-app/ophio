// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AgentChip } from "./AgentChip";

afterEach(cleanup);

it.each([
  ["Claude Code · Opus 5.5 · High", "Claude · Opus 5.5 · High"],
  ["Codex · GPT-6.1 Sol Codex Max Preview · High", "GPT-6.1 Sol Codex Max Preview · High"],
  ["Codex · GPT-6.1 Sol Codex Max Preview", "GPT-6.1 Sol Codex Max Preview"],
  ["Codex · GPT-6-Astra · Extra high", "Codex · GPT-6-Astra · Extra high"],
])("keeps model and effort in %s and names the full agent accessibly", (label, visible) => {
  const open = vi.fn();
  render(<AgentChip label={label} lead="Agent for this task:" onClick={open} />);
  const chip = screen.getByRole("button", { name: `Agent for this task: ${label}` });
  expect(chip.textContent).toBe(visible);
  expect(chip.getAttribute("aria-haspopup")).toBe("dialog");
  fireEvent.click(chip);
  expect(open).toHaveBeenCalledOnce();
});
