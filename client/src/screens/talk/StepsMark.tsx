// SPDX-License-Identifier: Apache-2.0
//
// The mark that leads a closed row of steps: it says whether they are still
// going, so a finished row no longer looks like a running one. The spinner
// only while a step runs and the computer is reachable; a quiet check once
// they are done; a stop mark when the run was stopped under them.

import { CircleCheck, CircleStop, Wrench } from "lucide-react";

export function StepsMark({ running, online, stopped }: { running: boolean; online: boolean; stopped: boolean }) {
  if (running && online) return <span className="stateline__spinner talk-steps__spin" role="img" aria-label="Running" />;
  // Offline, the row says when a step was last reported running.
  if (running) return <Wrench className="talk-steps__lead" aria-hidden strokeWidth={2.1} />;
  if (stopped) return <CircleStop className="talk-steps__lead" role="img" aria-label="Stopped" strokeWidth={2.1} />;
  return <CircleCheck className="talk-steps__lead" role="img" aria-label="Done" strokeWidth={2.1} />;
}
