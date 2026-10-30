// SPDX-License-Identifier: Apache-2.0
// Validate observations independently of the guest and phone automation.
export function lockPassed(result) {
  return result.lockConfirmed === true && result.sent === false &&
    result.samples.length === 4 && result.samples.every(sample => {
      const views = Object.values(sample.control);
      return sample.acks.length === 0 && views.length > 0 && views.every(view =>
        view.desktop_lock === "locked" && view.input_state === "paused" &&
        (!view.lease || view.lease.state === "revoked"));
    });
}
