// SPDX-License-Identifier: Apache-2.0
import { useMemo } from "react";
import { FileText } from "lucide-react";
import { useApp } from "../../app/context";
import { Screen } from "../../shell/Screen";
import { Age } from "../../design/Age";
import { TextField } from "../../design/Field";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { NotAllowed } from "../../shell/NotAllowed";
import type { AgentFile } from "../../protocol/types";
import { useStore } from "../../state/store";
import { agentFileGroups, basename } from "./model";
import {
  ConnectionNote,
  groupIcon,
  LoadError,
  useAgentFiles,
  useLibrary,
  useLibraryScope,
  useScreenBody,
} from "./shared";
import type { RouteProps } from "../types";
import { Checking, DisabledSection, shownDisabled } from "./Disabled";
import { listedDisabled, useDisabled } from "../../app/disabled";
import "./library.css";

/** One kind of agent file, by where it applies: with a project chosen,
 * only that project's. */
export default function AgentFiles({ route }: RouteProps<"agent_files">) {
  const bodyRef = useScreenBody();
  const { ui } = useApp(),
    store = useLibrary(),
    q = useAgentFiles(),
    scope = useLibraryScope()?.project_id,
    disabled = useDisabled();
  const group = agentFileGroups.find((g) => g.group === route.group)!;
  const key = `agent_files_${route.group}`,
    text = useStore(store, (s) => s.forms[key] ?? ""),
    needle = text.trim().toLowerCase();
  const all = useMemo(
    () =>
      q.cached?.files.filter(
        (f) =>
          f.group === route.group &&
          (!scope || f.project_id === scope) &&
          // Disabled in Øphio, a file is listed in its own section below.
          (!disabled.supported || !disabled.isDisabled(f.disable?.state === "can" ? f.disable.key ?? f.host_path ?? "" : f.host_path ?? "", listedDisabled(f.disable), f.disable?.state === "can" && !!f.disable.key)),
      ) ?? [],
    [q.cached, route.group, scope, disabled.isDisabled],
  );
  const scopes = useMemo(() => {
    const shown = all.filter(
      (f) =>
        !needle ||
        [f.title, f.description, f.shown, f.scope].some((s) =>
          s?.toLowerCase().includes(needle),
        ),
    );
    const by = new Map<string, AgentFile[]>();
    for (const f of shown) by.set(f.scope, [...(by.get(f.scope) ?? []), f]);
    return [...by];
  }, [all, needle]);
  return (
    <Screen bodyRef={bodyRef} className="library-screen" title={group.label}>
      <div className="lib-stack">
        <ConnectionNote at={q.cached?.at} />
        {!q.allowed && (
          <NotAllowed what="read the Library" permission="library-read" />
        )}
        {q.error && <LoadError error={q.error} retry={q.refresh} />}
        <p className="lib-meta">
          {group.about}.
          {q.cached && (
            <>
              {" "}
              Checked <Age at={q.cached.at} suffix=" ago" />.
            </>
          )}
        </p>
        {all.length > 8 && (
          <TextField
            label={`Find ${group.label.toLowerCase()}`}
            placeholder="Name, description or where it applies"
            value={text}
            onChange={(v) => store.form(key, v)}
            type="search"
          />
        )}
        {q.loading && !q.cached && <Checking what={group.label.toLowerCase()} />}
        {q.loading && q.cached && <Checking refreshing />}
        {scopes.map(([scope, files]) => (
          <Section key={scope} title={scope}>
            <div className="group">
              {files.map((f) => {
                const name = f.title ?? basename(f.shown);
                return (
                  <ListRow
                    key={`${f.root_id}/${f.path}`}
                    icon={groupIcon[f.group] ?? FileText}
                    title={name}
                    subtitle={
                      f.description ?? (name === f.shown ? undefined : f.shown)
                    }
                    onClick={() =>
                      ui.push({ screen: "file", rootId: f.root_id, path: f.path })
                    }
                    chevron
                  />
                );
              })}
            </div>
          </Section>
        ))}
        {q.cached && scopes.length === 0 && !(!needle && shownDisabled(disabled.items, scope, route.group).length) && (
          <Empty icon={groupIcon[route.group]} title={needle ? "Nothing matches" : `No ${group.label.toLowerCase()}`}>
            {needle
              ? "Try another name or word."
              : "None of the agents on the computer keep any."}
          </Empty>
        )}
        <DisabledSection projectId={scope} only={route.group} />
        {q.cached?.truncated && (
          <p className="lib-meta">
            The computer keeps more agent files than Library lists.
          </p>
        )}
      </div>
    </Screen>
  );
}
