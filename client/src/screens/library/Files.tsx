// SPDX-License-Identifier: Apache-2.0
//
// Library's files: the agent files by kind, the folders shared from the
// computer, search, favourites and what was opened lately; with a project
// chosen, only those in its folders.

import { PRODUCT_NAME } from "../../brand";
import { useEffect, useState } from "react";
import { FileText, FolderOpen, Search, Star, ArrowDownUp } from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useStore } from "../../state/store";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Empty, ListRow, Section } from "../../design/ListRow";
import type { Project, SearchHit } from "../../protocol/types";
import {
  agentFileGroups,
  basename,
  category,
  rootLabel,
  type Filter,
} from "./model";
import {
  Filters,
  groupIcon,
  LoadError,
  StorageNote,
  useAgentFiles,
  useLibrary,
  useRoots,
} from "./shared";
import type { Remembered } from "./drafts";
import { NotAllowed } from "../../shell/NotAllowed";
import { RecoveryList } from "./RecoveryList";
import { Checking, DisabledSection } from "./Disabled";
import { listedDisabled, useDisabled } from "../../app/disabled";
import "./library.css";

export function LibraryFiles({ scope }: { scope: Project | null }) {
  const { ui, session, profile } = useApp(),
    link = useLink(),
    store = useLibrary(),
    every = useRoots(),
    agents = useAgentFiles(),
    disabled = useDisabled();
  const roots = scope ? every.filter((r) => r.project_id === scope.project_id) : every;
  const [filter, setFilter] = useState<Filter>("All"),
    [hits, setHits] = useState<SearchHit[] | null>(null),
    [searchAt, setSearchAt] = useState<number | null>(null),
    [resultText, setResultText] = useState(""),
    [error, setError] = useState<string | null>(null),
    [busy, setBusy] = useState(false),
    [retry, setRetry] = useState(0);
  const text = useStore(store, (s) => s.forms.search ?? ""),
    favourites = useStore(store, (s) => s.favourites),
    recent = useStore(store, (s) => s.recent),
    rootCache = useStore(store, (s) => s.roots);
  const canRead = useHost((s) => s.grants?.library_read),
    online = link.state === "online";
  useEffect(() => {
    if (!text.trim()) {
      setHits(null);
      setError(null);
      return;
    }
    if (!online) {
      setBusy(false);
      return;
    }
    let cancelled = false;
    setBusy(true);
    const timer = setTimeout(() => {
      void session
        .query({
          kind: "library_search",
          root_id: null,
          text: text.trim(),
          limit: 80,
        })
        .then((r) => {
          if (!cancelled) {
            setHits(r.hits);
            setResultText(text.trim());
            setSearchAt(Date.now());
            setError(null);
          }
        })
        .catch((e: unknown) => {
          if (!cancelled) setError(describeError(e, profile.name));
        })
        .finally(() => {
          if (!cancelled) setBusy(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [text, online, session, profile.name, retry]);
  const open = (rootId: string, path: string) =>
    ui.push({ screen: "file", rootId, path });
  const matches = (rootId: string, path: string) =>
    filter === "All" ||
    category(path, roots.find((r) => r.root_id === rootId)?.kind) === filter;
  const remembered = (items: Remembered[], fav: boolean) =>
    items
      .filter(
        (r) =>
          roots.some((x) => x.root_id === r.rootId) &&
          matches(r.rootId, r.path),
      )
      .map((r) => (
        <ListRow
          key={`${r.rootId}/${r.path}`}
          icon={fav ? Star : FileText}
          title={basename(r.path)}
          subtitle={`${roots.find((x) => x.root_id === r.rootId)?.label} › ${r.path}`}
          onClick={() => open(r.rootId, r.path)}
          chevron
        />
      ));
  // Agent files are listed by kind, not by the folders that keep them.
  const folders = roots.filter((r) => r.kind !== "agent_files");
  const kinds = agentFileGroups
    .map((g) => ({
      ...g,
      count:
        agents.cached?.files.filter(
          (f) =>
            f.group === g.group &&
            (!scope || f.project_id === scope.project_id) &&
            // Disabled in Øphio, a file is listed in its own section.
            (!disabled.supported || !disabled.isDisabled(f.disable?.state === "can" ? f.disable.key ?? f.host_path ?? "" : f.host_path ?? "", listedDisabled(f.disable), f.disable?.state === "can" && !!f.disable.key)),
        ).length ?? 0,
    }))
    .filter((g) => g.count > 0 && (filter === "All" || g.filter === filter));
  const results =
    resultText === text.trim()
      ? hits?.filter(
          (h) =>
            roots.some((r) => r.root_id === h.root_id) &&
            matches(h.root_id, h.path),
        )
      : null;
  const grouped = groupSearchHits(results ?? []);
  return (
    <div className="lib-stack">
      <StorageNote />
      {!text.trim() &&
        canRead !== false &&
        (kinds.length > 0 ||
          agents.error ||
          (agents.loading && !agents.cached)) && (
          <Section title="Agent files">
            {agents.error && (
              <LoadError error={agents.error} retry={agents.refresh} />
            )}
            {kinds.length > 0 && (
              <div className="group">
                {kinds.map((g) => (
                  <ListRow
                    key={g.group}
                    icon={groupIcon[g.group]}
                    title={g.label}
                    subtitle={g.about}
                    // Files, which can differ from the agents and skills Library lists.
                    meta={`${g.count.toLocaleString()} ${g.count === 1 ? "file" : "files"}`}
                    onClick={() =>
                      ui.push({ screen: "agent_files", group: g.group })
                    }
                    chevron
                  />
                ))}
              </div>
            )}
            {agents.loading && !agents.cached && <Checking />}
            {agents.loading && agents.cached && <Checking refreshing />}
          </Section>
        )}
      {!text.trim() && canRead !== false && (
        <Section title="Folders on the computer">
          <div className="group">
            {folders
              .filter(
                (r) =>
                  filter === "All" ||
                  r.kind === "project" ||
                  category("", r.kind) === filter,
              )
              .map((r) => (
                <ListRow
                  key={r.root_id}
                  icon={FolderOpen}
                  title={r.label}
                  subtitle={
                    <>
                      <span>
                        {rootLabel[r.kind]} ·{" "}
                        {r.writable ? "Read and write" : "Read only"}
                      </span>
                      <span className="lib-path mono">{r.host_path}</span>
                    </>
                  }
                  onClick={() =>
                    ui.push({
                      screen: "folder",
                      rootId: r.root_id,
                      path: "",
                    })
                  }
                  chevron
                />
              ))}
          </div>
          {folders.length === 0 &&
            (scope && rootCache ? (
              <Empty icon={FolderOpen} title="No Library folders here">
                Library has no folders from {scope.name}. Home lists every
                folder shared in {PRODUCT_NAME}.
              </Empty>
            ) : (
              <Empty
                icon={FolderOpen}
                title={rootCache ? "No Library folders" : "Loading Library…"}
              >
                Choose folders to share in {PRODUCT_NAME} on the computer.
              </Empty>
            ))}
        </Section>
      )}
      <RecoveryList />
      <Filters value={filter} onChange={setFilter} />
      {error && (
        <LoadError error={error} retry={() => setRetry((n) => n + 1)} />
      )}
      {canRead === false ? (
        <NotAllowed what="read the Library" permission="library-read" />
      ) : text.trim() ? (
        <Section title="Search results">
          {results && searchAt && (
            <p className="lib-meta">
              Checked <Age at={searchAt} suffix=" ago" />
            </p>
          )}
          {busy && online && (
            <p className="lib-meta" role="status">
              Searching the computer…
            </p>
          )}
          {grouped.size ? (
            <div className="group">
              {[...grouped].map(([key, fileHits]) => (
                <SearchFileRow
                  key={`${resultText}/${key}`}
                  hits={fileHits}
                  rootLabel={roots.find((r) => r.root_id === fileHits[0]!.root_id)?.label ?? "Folder"}
                  open={() => open(fileHits[0]!.root_id, fileHits[0]!.path)}
                />
              ))}
            </div>
          ) : (
            !busy && (
              <Empty
                icon={Search}
                title={
                  online ? "No matching files" : "Search needs the computer"
                }
              >
                Try a file name or a few words from its contents.
              </Empty>
            )
          )}
        </Section>
      ) : (
        <>
          {filter === "All" && favourites.length > 0 && (
            <Section title="Favourites">
              <div className="group">{remembered(favourites, true)}</div>
            </Section>
          )}
          {recent.length > 0 && (
            <Section title="Recently opened">
              <div className="group">{remembered(recent, false)}</div>
            </Section>
          )}
        </>
      )}
      {!text.trim() && canRead !== false && (
        <DisabledSection projectId={scope?.project_id} />
      )}
      <Button
        variant="quiet"
        onClick={() => ui.push({ screen: "recently_deleted" })}
      >
        Recently deleted
      </Button>
      <Button
        variant="quiet"
        icon={ArrowDownUp}
        onClick={() => ui.push({ screen: "transfers" })}
      >
        Transfers
      </Button>
    </div>
  );
}

/** One file stays one result, even when its name and many lines match. */
export function groupSearchHits(hits: SearchHit[]): Map<string, SearchHit[]> {
  const grouped = new Map<string, SearchHit[]>();
  for (const hit of hits) {
    const key = JSON.stringify([hit.root_id, hit.path]);
    const file = grouped.get(key);
    if (file) file.push(hit);
    else grouped.set(key, [hit]);
  }
  return grouped;
}

export function SearchFileRow({ hits, rootLabel, open }: { hits: SearchHit[]; rootLabel: string; open: () => void }) {
  const [expanded, setExpanded] = useState(false);
  const first = hits[0]!;
  const lines = hits.filter((hit) => hit.line !== undefined);
  const visible = expanded ? lines : lines.slice(0, 3);
  const remaining = lines.length - 3;
  return (
    <div className="lib-search-file">
      <ListRow
        icon={FileText}
        title={basename(first.path)}
        subtitle={
          <>
            <span className="lib-path">{rootLabel} › {first.path}</span>
            <span className="lib-meta">{lines.length ? `${lines.length} matching ${lines.length === 1 ? "line" : "lines"}` : "Name matches"}</span>
            {visible.map((hit) => (
              <span className="lib-snippet" key={hit.line}>Line {hit.line}: {hit.snippet}</span>
            ))}
          </>
        }
        onClick={open}
        chevron
      />
      {remaining > 0 && (
        <Button variant="quiet" onClick={() => setExpanded((value) => !value)} aria-expanded={expanded}>
          {expanded ? "Show fewer lines" : `Show ${remaining} more matching ${remaining === 1 ? "line" : "lines"}`}
        </Button>
      )}
    </div>
  );
}
