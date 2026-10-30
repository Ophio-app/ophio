// SPDX-License-Identifier: Apache-2.0
//
// What each result from the computer's contribution receiver means to the
// person (CH-016's copy deck, fitted to this receiver). Every message says
// what happened, what didn't, and what to do next. Values from the handoff
// itself are never put into a sentence: they are shown as data, apart from
// the copy, so a handoff can't write Ophio's words for it.

import { PRODUCT_NAME as APP } from "../../brand";
import type { ContributionCode, ContributionView } from "../../protocol/types";

/** Where the flow is, as the screen draws it. */
export type Stage =
  /** Choosing where it goes, then the rules and the check. */
  | "review"
  /** Downloading, or checking what was downloaded. */
  | "progress"
  /** The project is on the computer, paused. */
  | "ready"
  | "canceled"
  /** A restart left it unclear whether the project was added. */
  | "reconcile"
  | "blocked";

export function stageOf(view: ContributionView): Stage {
  switch (view.code) {
    case "awaiting_local_approval":
    case "trust_required":
      return "review";
    case "acquiring":
    case "verifying":
      return "progress";
    case "prepared":
    case "reopened":
      return "ready";
    case "canceled":
      return "canceled";
    case "reconcile_required":
      return "reconcile";
    case "freshness_unknown":
      // A prepared result asked about after its check has lapsed still names
      // the project it added; the lapse only stops another preparation.
      return view.project_id ? "ready" : "blocked";
    default:
      return "blocked";
  }
}

/** What a blocker's buttons do. */
export type Remedy =
  /** Ask the computer to check the handoff again, as it is. */
  | "review"
  /** The person checks it's from Clankerhub again, then Ophio downloads. */
  | "check"
  | "folder"
  | "computer"
  | "updates"
  | "dismiss";

export interface Blocker {
  heading: string;
  body: string;
  /** Under the body, in a quieter panel. */
  note?: string;
  primary: { label: string; remedy: Remedy };
  secondary?: { label: string; remedy: Remedy };
  /** The fixed code name, for reporting. Shown for failed checks and the unknown. */
  reference?: string;
  /** A failed check or an unknown result, rather than something to wait out or choose around. */
  tone: "warn" | "stop";
}

export interface Facts {
  computer: string;
  /** The Library folder's label, when one is chosen. */
  folder: string | null;
  /** Ophio had started getting the source, so "nothing was downloaded" may not be true. */
  started: boolean;
}

const DISMISS = { label: "Dismiss", remedy: "dismiss" } as const;
const FRESH = "Press Open in Ophio on the game’s Clankerhub page for a fresh one.";

function nothing(f: Facts): string {
  return f.started ? "Nothing was added or run." : "Nothing was downloaded.";
}

/** A check that failed: plain detail, the code for reporting, and no retry,
 * since the same handoff fails the same way. */
function failedCheck(code: string, detail: string, f: Facts): Blocker {
  return {
    heading: `This handoff didn’t pass ${APP}’s checks`,
    body: `${detail} ${nothing(f)}`,
    note: FRESH,
    primary: DISMISS,
    reference: code,
    tone: "stop",
  };
}

const unreadable = (detail: string, f: Facts): Blocker => ({
  heading: `${APP} can’t read this handoff`,
  body: `${detail} ${nothing(f)}`,
  note: FRESH,
  primary: DISMISS,
  tone: "stop",
});

/** The blocker for a receiver code, or for any code this version doesn't
 * know, which stops rather than guessing. */
export function blockerFor(code: ContributionCode | string, f: Facts): Blocker {
  const folder = f.folder ?? "the folder you chose";
  switch (code as ContributionCode) {
    case "host_unavailable":
      return {
        heading: "This was checked on another computer",
        body: `${APP} checked this handoff on a different computer from ${f.computer}. ${nothing(f)}`,
        primary: { label: `Check it on ${f.computer}`, remedy: "review" },
        secondary: { label: "Choose another computer", remedy: "computer" },
        tone: "warn",
      };
    case "freshness_unknown":
      return {
        heading: "Check it again",
        body: `Your check that this came from Clankerhub is more than 5 minutes old, or this phone’s clock and ${f.computer}’s disagree. ${nothing(f)}`,
        primary: { label: "Check again", remedy: "check" },
        tone: "warn",
      };
    case "not_yet_valid":
      return {
        heading: "Check the date and time",
        body: `This handoff isn’t valid yet, which usually means the clock on ${f.computer} is behind. Set it to update automatically, then try again. ${nothing(f)}`,
        primary: { label: "Try again", remedy: "review" },
        tone: "warn",
      };
    case "expired":
      return {
        heading: "This handoff expired",
        body: `Handoffs only work for a short time. ${nothing(f)}`,
        note: FRESH,
        primary: DISMISS,
        tone: "warn",
      };
    case "untrusted_packet":
      return {
        heading: `${APP} couldn’t confirm this came from Clankerhub`,
        body: `${nothing(f)} If you didn’t just press Open in Ophio, you can ignore this.`,
        primary: DISMISS,
        tone: "stop",
      };
    case "invalid_json":
    case "invalid_record":
      return unreadable("It isn’t a valid Clankerhub handoff.", f);
    case "unsupported_version":
      return {
        heading: `${APP} can’t read this handoff`,
        body: `It uses a format this version of ${APP} doesn’t know. Updating ${APP} on ${f.computer} may help. ${nothing(f)}`,
        primary: { label: "Check for updates", remedy: "updates" },
        secondary: DISMISS,
        tone: "warn",
      };
    case "limit_exceeded":
      return f.started
        ? {
          heading: "This source is too big to open here",
          body: `It’s larger than ${APP} downloads for a contribution: up to 512 MB and 20,000 files. Nothing was added or run.`,
          primary: DISMISS,
          tone: "stop",
        }
        : unreadable(`It’s larger than handoffs are allowed to be, or ${APP} is holding too many earlier ones.`, f);
    case "invalid_lifetime":
      return failedCheck(code, "Its start and expiry times aren’t allowed.", f);
    case "handoff_digest_mismatch":
      return failedCheck(code, "Its contents don’t match the handoff Clankerhub approved.", f);
    case "catalog_digest_mismatch":
      return failedCheck(code, "It doesn’t match the listing you checked.", f);
    case "release_mismatch":
      return failedCheck(code, "It names a different game, release or source from the one you checked.", f);
    case "stale_policy":
      return {
        heading: "The game’s rules don’t match",
        body: `The rule files at this revision aren’t the ones Clankerhub reviewed, so ${APP} stopped before using them. Nothing was added or run.`,
        note: FRESH,
        primary: DISMISS,
        reference: code,
        tone: "stop",
      };
    case "compatibility_unknown":
      return {
        heading: `${APP} can’t confirm it supports this`,
        body: `The handoff doesn’t say which version of the handoff format it needs, so ${APP} stopped. ${nothing(f)}`,
        primary: DISMISS,
        reference: code,
        tone: "warn",
      };
    case "compatibility_unsupported":
      return {
        heading: `Update ${APP} to open this`,
        body: `This handoff needs something ${APP} on ${f.computer} doesn’t support yet. ${nothing(f)}`,
        primary: { label: "Check for updates", remedy: "updates" },
        secondary: DISMISS,
        reference: code,
        tone: "warn",
      };
    case "root_changed":
      return {
        heading: "That folder changed",
        body: `${folder} on ${f.computer} moved, changed, or no longer lets ${APP} write the whole project there. ${nothing(f)}`,
        primary: { label: "Choose another folder", remedy: "folder" },
        secondary: { label: "Try again", remedy: "review" },
        tone: "warn",
      };
    case "id_conflict":
      return {
        heading: `This handoff clashes with one ${APP} already has`,
        body: `Two different handoffs used the same ID. ${APP} kept the first and stopped. ${nothing(f)}`,
        note: FRESH,
        primary: DISMISS,
        reference: code,
        tone: "stop",
      };
    case "workspace_conflict":
      return {
        heading: `You already have this game in ${folder}`,
        body: `${APP} already opened this repository there on ${f.computer}, at another revision or with other rules. ${nothing(f)}`,
        note: `There’s no reset or update here. ${APP} never changes files in a workspace you already have. To get this revision too, choose another folder.`,
        primary: { label: "Choose another folder", remedy: "folder" },
        secondary: DISMISS,
        tone: "warn",
      };
    case "unsafe_repository":
      return {
        heading: `${APP} won’t download from this address`,
        body: `It isn’t a plain public HTTPS address of the kind ${APP} downloads from. ${nothing(f)}`,
        primary: DISMISS,
        reference: code,
        tone: "stop",
      };
    case "private_destination":
      return {
        heading: `${APP} won’t download from this address`,
        body: `It leads to a private network, so ${APP} stopped. ${nothing(f)}`,
        primary: DISMISS,
        reference: code,
        tone: "stop",
      };
    case "unsafe_path":
      return {
        heading: `This source has files ${APP} won’t write`,
        body: `Some of its file or folder names aren’t safe to write on ${f.computer}, so ${APP} stopped before adding the project. Nothing was run.`,
        primary: DISMISS,
        reference: code,
        tone: "stop",
      };
    case "unsafe_object":
      return {
        heading: `This source has content ${APP} won’t use`,
        body: `It contains things ${APP} doesn’t accept here, such as links to other files or repositories, so ${APP} stopped before adding the project. Nothing was run.`,
        primary: DISMISS,
        reference: code,
        tone: "stop",
      };
    case "acquisition_failed":
      return {
        heading: "The download didn’t finish",
        body: `${APP} couldn’t get the source from where Clankerhub says it is. Anything it got so far stays in its own folder on ${f.computer}. Nothing was added or run.`,
        primary: { label: "Try again", remedy: "check" },
        secondary: DISMISS,
        tone: "warn",
      };
    case "disk_full":
      return {
        heading: `Not enough space on ${f.computer}`,
        body: `${APP} needs at least 2 GB free in ${folder} to download a game. ${nothing(f)}`,
        primary: { label: "Choose another folder", remedy: "folder" },
        secondary: { label: "Try again", remedy: f.started ? "check" : "review" },
        tone: "warn",
      };
    case "busy":
      return {
        heading: `${f.computer} is busy`,
        body: `It’s already getting two other projects. Try again when one finishes. ${nothing(f)}`,
        primary: { label: "Try again", remedy: "check" },
        tone: "warn",
      };
    default:
      return {
        heading: "Something unexpected",
        body: `${APP} got a result it doesn’t recognize, so it stopped. ${f.started ? "Nothing was added or run." : "Nothing was downloaded or started."}`,
        primary: DISMISS,
        reference: String(code).slice(0, 40),
        tone: "stop",
      };
  }
}

/** Roles the handoff gives its rule files, in words. */
export function roleLabel(role: string): string {
  switch (role) {
    case "instructions": return "Agent instructions";
    case "contribution": return "Contributing";
    case "model-policy": return "AI policy";
    case "design": return "Design";
    case "setup": return "Setup";
    default: return "Rules";
  }
}

/** A revision as people compare it: its first 7 characters. */
export const shortRevision = (commit: string): string => commit.slice(0, 7);

/** A repository address without its scheme, as Clankerhub's listing shows it. */
export const repositoryLabel = (url: string): string => url.replace(/^https:\/\//, "");
