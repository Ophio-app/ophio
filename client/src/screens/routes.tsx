// SPDX-License-Identifier: Apache-2.0
//
// Logical destination → screen. Each screen loads on first use so the
// everyday Talk path starts fast and heavy views (terminal, editor, desktop)
// cost nothing until opened. Each folder under screens/ owns its screens.

import {
  lazy,
  Suspense,
  type ComponentType,
  type LazyExoticComponent,
} from "react";
import type { Route } from "../app/ui";
import { ContextHeader } from "../shell/ContextHeader";
import { Screen } from "../shell/Screen";
import type { RouteProps } from "./types";

type Screens = {
  [S in Route["screen"]]: LazyExoticComponent<ComponentType<RouteProps<S>>>;
};

const screens: Screens = {
  talk: lazy(() => import("./talk/Talk")),
  review: lazy(() => import("./talk/Review")),
  annotate: lazy(() => import("./annotate/Annotate")),
  control: lazy(() => import("./control/Control")),
  desktop: lazy(() => import("./control/Desktop")),
  computer_use: lazy(() => import("./control/ComputerUse")),
  terminal: lazy(() => import("./control/Terminal")),
  system: lazy(() => import("./control/SystemScreen")),
  work: lazy(() => import("./work/Work")),
  agents: lazy(() => import("./work/Agents")),
  receipt: lazy(() => import("./work/Receipt")),
  next_up: lazy(() => import("./work/NextUp")),
  attention: lazy(() => import("./work/Attention")),
  needs_you: lazy(() => import("./work/NeedsYou")),
  inbox: lazy(() => import("./work/Inbox")),
  library: lazy(() => import("./library/Library")),
  folder: lazy(() => import("./library/Folder")),
  agent_files: lazy(() => import("./library/AgentFiles")),
  file: lazy(() => import("./library/File")),
  conflict: lazy(() => import("./library/Conflict")),
  transfers: lazy(() => import("./transfers/Transfers")),
  connections: lazy(() => import("./settings/Connections")),
  recently_deleted: lazy(() => import("./library/RecentlyDeleted")),
  instructions: lazy(() => import("./library/Instructions")),
  reusable: lazy(() => import("./library/Reusable")),
  computer_use_review: lazy(() => import("./control/ComputerReview")),
  settings: lazy(() => import("./settings/Settings")),
  accounts: lazy(() => import("./settings/Accounts")),
  agent_logins: lazy(() => import("./settings/AgentLogins")),
  voice: lazy(() => import("./control/Voice")),
  windows: lazy(() => import("./control/Windows")),
  library_recovery: lazy(() => import("./library/Recovery")),
  stages: lazy(() => import("./work/Stages")),
  github: lazy(() => import("./github/Github")),
  github_list: lazy(() => import("./github/GithubList")),
  github_new: lazy(() => import("./github/GithubNew")),
  findings: lazy(() => import("./findings/Findings")),
  findings_new: lazy(() => import("./findings/NewFindings")),
  checklist: lazy(() => import("./LegacyChecklistRedirect")),
  todo: lazy(() => import("./checklist/Todo")),
  contribution: lazy(() => import("./contribute/Contribution")),
  contribution_rules: lazy(() => import("./contribute/ContributionRules")),
};

export function RouteView({ route }: { route: Route }) {
  const C = screens[route.screen] as LazyExoticComponent<
    ComponentType<{ route: Route }>
  >;
  return (
    <Suspense fallback={<Loading />}>
      <C route={route} />
    </Suspense>
  );
}

// A destination's root keeps its header in place while it loads, but inert: a
// sheet opened from it would close when the real screen replaces it, so a tap
// lands on the real header instead. Below it, the outline of a screen, still:
// a title, a line and a few rows, faded in only when loading takes long
// enough to notice, so a quick load shows no flash.
function Loading() {
  return (
    <Screen title="" header={<ContextHeader inert />}>
      <div className="screen__loading" aria-busy="true">
        <p className="visually-hidden">Loading…</p>
        <div className="skeleton" aria-hidden>
          <span className="skeleton__bar skeleton__bar--title" />
          <span className="skeleton__bar skeleton__bar--meta" />
          <div className="skeleton__group">
            {[0, 1, 2].map((i) => (
              <div className="skeleton__row" key={i}>
                <span className="skeleton__icon" />
                <span className="skeleton__lines">
                  <span className="skeleton__bar" />
                  <span className="skeleton__bar skeleton__bar--short" />
                </span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </Screen>
  );
}
