// SPDX-License-Identifier: Apache-2.0
//
// The computer a part of the screen is about, for a report filed from it:
// another computer's part of All's lists names its own, and a report from
// there goes through, and describes, that computer. Elsewhere it is none,
// and a report goes through the computer on screen.

import { createContext, useContext } from "react";

export const ReportComputer = createContext<string | null>(null);

export function useReportComputer(): string | null {
  return useContext(ReportComputer);
}
