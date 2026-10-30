// SPDX-License-Identifier: Apache-2.0
// Kept in step with protocol::reports; checked by the diagnostics tests.
export const REPORT_SCHEMA = 1;
export const MAX_REPORT_BYTES = 900 * 1024;
export const MAX_SCREENSHOT_BYTES = 512 * 1024;
export const MAX_REPORT_WORDS = 2000;
export const MAX_TRAIL_EVENTS = 400;
export const PHONE_TRAIL_MS = 10 * 60 * 1000;
export const MAX_REPORT_CODE = 160;
export const MAX_FRAMES = 40;
export const MAX_STATUS_IDS = 200;
export const REPORTS_FEATURE = "reports_v1";
/** A computer whose owner answers the problem reports others send. */
export const REPORT_INBOX_FEATURE = "report_inbox_v1";
/** A computer whose report fixer runs, taking only what the owner chooses. */
export const REPORT_LOOK_INTO_FEATURE = "report_look_into_v1";
