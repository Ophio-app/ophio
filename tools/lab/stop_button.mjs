// SPDX-License-Identifier: Apache-2.0
// Recognise one task's Stop exactly: its title, then only the display the
// button itself shows. Self-contained so the phone helper can send it into
// the page as source.
export function isTaskStop(label, shownDisplay, title) {
  return typeof label === "string" && typeof title === "string" &&
    label === `Stop · ${title}${typeof shownDisplay === "string" ? shownDisplay : ""}`;
}
