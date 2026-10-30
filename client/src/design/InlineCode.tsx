// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";

/**
 * Code inside a sentence: a command, a file name, a revision. It keeps its look
 * but takes no role of its own, so a phone screen reader reads the sentence
 * around it as one piece instead of stopping before, on and after it.
 */
export function InlineCode({ className, children }: { className?: string; children: ReactNode }) {
  return <code role="none" className={className}>{children}</code>;
}
