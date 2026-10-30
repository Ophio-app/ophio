// SPDX-License-Identifier: Apache-2.0

import { clockTime, shortAge, spokenAge, useNow } from "./time";

/** Relative time with a one-second present window, including future deadlines. */
export function relativeAge(at: number, now: number, suffix = "", nowText = "now") {
  const delta = now - at;
  const ms = Math.abs(delta);
  if (ms < 1000) return { text: nowText, spoken: "just now" };
  const short = ms < 5000 ? `${Math.floor(ms / 1000)} s` : shortAge(ms);
  const spoken = ms < 5000 ? `${Math.floor(ms / 1000)} second${ms < 2000 ? "" : "s"} ago` : spokenAge(ms);
  return delta < 0
    ? { text: `in ${short}`, spoken: `in ${spoken.replace(/ ago$/, "")}` }
    : { text: `${short}${suffix}`, spoken };
}

/**
 * A live relative age with its exact time available to assistive tech and on hover.
 * With `clock`, the clock time is shown before it in one element, so a screen
 * reader reads the pair as one value rather than in pieces.
 */
export function Age({ at, suffix = "", now: nowText = "now", className, clock = false }: { at: number; suffix?: string; now?: string; className?: string; clock?: boolean }) {
  const now = useNow();
  const age = relativeAge(at, now, suffix, nowText);
  const exact = clockTime(at, now);
  return (
    <time
      className={["num", className].filter(Boolean).join(" ")}
      dateTime={new Date(at).toISOString()}
      title={exact}
      aria-label={clock ? `${exact}, ${age.spoken}` : age.spoken}
    >
      {clock ? `${exact} (${age.text})` : age.text}
    </time>
  );
}
