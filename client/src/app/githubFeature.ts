// SPDX-License-Identifier: Apache-2.0
//
// Whether the computer links tasks to GitHub. Kept apart from github.ts so
// the session can check it without importing React.

export const GITHUB_FEATURE = "github_v1";
export const GITHUB_UPDATE = "Update Øphio on this computer to link tasks to GitHub. Nothing will be sent with a link it can’t follow.";

/** Reviews with profiles, a most-issues limit and a choice of what follows. */
export const GITHUB_REVIEW_FEATURE = "github_review_v1";
export const GITHUB_REVIEW_UPDATE = "Update Øphio on this computer to choose a review’s profile, how many issues it files and what happens next.";

/** Filing issues from the phone, and an issue's own reviewer. */
export const GITHUB_ISSUES_FEATURE = "github_issues_v1";
export const GITHUB_ISSUES_UPDATE = "Update Øphio on this computer to file issues from your phone and have another agent review an issue’s fix.";

/** Reviewers who use the app on a screen, as computer use. */
export const GITHUB_UAT_FEATURE = "github_uat_v1";
export const GITHUB_UAT_UPDATE = "Update Øphio on this computer for reviewers who use the app on its screen.";

/** Run all on the Issues tab, and moving an issue between groups. */
export const GITHUB_RUN_FEATURE = "github_run_v1";
export const GITHUB_RUN_UPDATE = "Update Øphio on this computer to run every issue the agent can take with one tap.";

/** Closing an issue from the phone. */
export const GITHUB_CLOSE_FEATURE = "github_close_v1";
export const GITHUB_CLOSE_UPDATE = "Update Øphio on this computer to close issues from your phone. Until then, close it on GitHub.";

/** Picking several pull requests to test together and merge in turn. */
export const GITHUB_TOGETHER_FEATURE = "github_together_v1";
