// SPDX-License-Identifier: Apache-2.0
import type { TerminalInfo } from "../../protocol/types";

/** Use the host inventory, including ended sessions hidden from the list, so
 * removing an old row does not renumber another open terminal. */
export function terminalTitle(terminal: TerminalInfo, inventory: TerminalInfo[]): string {
  const sameName = inventory.filter(t => t.title === terminal.title)
    .sort((a, b) => a.created_at - b.created_at || a.terminal_id.localeCompare(b.terminal_id));
  const index = sameName.findIndex(t => t.terminal_id === terminal.terminal_id);
  return index > 0 ? `${terminal.title} (${index + 1})` : terminal.title;
}
