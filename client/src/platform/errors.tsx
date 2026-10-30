// SPDX-License-Identifier: Apache-2.0
import { announce } from "../design/Announcer";
import { Button } from "../design/Button";
import { Notice } from "../design/Notice";
import { Store, useStore } from "../state/store";

export interface PlatformProblem {
  operation: string;
  message: string;
}
export const platformProblems = new Store<PlatformProblem[]>([]);

/** Native failures often arrive both as a rejected call and a platform event. */
export function reportPlatformError(error: PlatformProblem): void {
  if (
    platformProblems
      .get()
      .some(
        (p) => p.operation === error.operation && p.message === error.message,
      )
  )
    return;
  platformProblems.set((all) => [...all, error]);
  announce(`${error.operation}: ${error.message}`);
}

export function PlatformNotices() {
  const problems = useStore(platformProblems, (s) => s);
  if (!problems.length) return null;
  return (
    <div className="platform-notices" aria-label="Device notices">
      {problems.map((problem) => (
        <Notice
          key={`${problem.operation}:${problem.message}`}
          tone="warn"
          title={problem.operation}
          action={
            <Button
              size="sm"
              variant="quiet"
              onClick={() =>
                platformProblems.set((all) => all.filter((p) => p !== problem))
              }
            >
              Dismiss
            </Button>
          }
        >
          {problem.message}
        </Notice>
      ))}
    </div>
  );
}
