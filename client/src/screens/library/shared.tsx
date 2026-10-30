// SPDX-License-Identifier: Apache-2.0
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Bot,
  Brain,
  Folder as FolderIcon,
  ListChecks,
  MoreHorizontal,
  RefreshCw,
  ScrollText,
  Sparkles,
  SquareSlash,
  type LucideIcon,
} from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { useStore } from "../../state/store";
import { libraryKey } from "../../state/projection";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Chip } from "../../design/Chip";
import { DiffLines } from "../../design/DiffLines";
import { Empty, ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { DiffRow } from "../../design/unifiedDiff";
import type { AgentFile, AgentFileGroup, Project } from "../../protocol/types";
import { libraryStore, fileKey } from "./drafts";
import { breadcrumbs, filters, type Filter } from "./model";
import { lineDiff } from "./diff";
import { useMay } from "../../shell/NotAllowed";

/**
 * The scrolling body of a Library destination. Focus on entry and on Back is
 * the shell's alone: it starts a new destination from its title and returns
 * to the control the person left, so nothing here moves focus.
 */
export function useScreenBody() {
  return useRef<HTMLDivElement>(null);
}

/** The project Library narrows to: the one chosen in the header, unless
 * that's Home, which holds everything. */
export function useLibraryScope(): Project | null {
  const selected = useUi((s) => s.projectId);
  return useHost((s) => {
    const p = selected ? s.projects[selected] : undefined;
    return p && !p.home ? p : null;
  }, shallowEqual);
}

export function useLibrary() {
  return libraryStore(useApp().profile.profileId);
}
export function useRoots() {
  const store = useLibrary(),
    roots = useHost((s) => s.libraryRoots),
    heard = useHost((s) => s.host !== null);
  const cached = useStore(store, (s) => s.roots);
  useEffect(() => {
    if (heard) store.cacheRoots(roots);
  }, [heard, roots, store]);
  return heard ? roots : (cached?.value ?? []);
}
export function useFolder(rootId: string, path: string) {
  const { session, profile } = useApp(),
    link = useLink(),
    store = useLibrary();
  const cached = useStore(store, (s) => s.listings[fileKey(rootId, path)]);
  const changes = useHost((s) => s.libraryRevisions);
  const [error, setError] = useState<string | null>(null),
    [loading, setLoading] = useState(false),
    [refresh, setRefresh] = useState(0);
  const online = link.state === "online",
    allowed = useMay("library_read");
  useEffect(() => {
    if (!online || !allowed) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    void session
      .query({ kind: "library_list", root_id: rootId, dir: path })
      .then((r) => {
        if (!cancelled) store.cacheListing(r.listing);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(describeError(e, profile.name));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [session, profile.name, rootId, path, online, allowed, store, changes, refresh]);
  return {
    cached,
    loading,
    error,
    allowed,
    refresh: () => setRefresh((n) => n + 1),
  };
}
export const groupIcon: Record<AgentFileGroup, LucideIcon> = {
  instructions: ScrollText,
  agents: Bot,
  skills: Sparkles,
  commands: SquareSlash,
  rules: ListChecks,
  memory: Brain,
};
/** The last agent files each profile heard of, kept while the app runs. */
const agentFiles = new Map<
  string,
  { files: AgentFile[]; truncated: boolean; at: number }
>();
/** Read the last scan without contacting the computer. */
export function cachedAgentFiles(profileId: string) { return agentFiles.get(profileId); }
/** Every Markdown file agents are given, wherever the computer keeps it. */
export function useAgentFiles(enabled = true) {
  const { session, profile } = useApp(),
    link = useLink();
  const roots = useHost((s) => s.libraryRoots);
  const [cached, setCached] = useState(() => agentFiles.get(profile.profileId)),
    [error, setError] = useState<string | null>(null),
    [loading, setLoading] = useState(false),
    [refresh, setRefresh] = useState(0);
  const online = link.state === "online",
    allowed = useMay("library_read");
  useEffect(() => {
    if (!enabled || !online || !allowed) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    void session
      .query({ kind: "agent_files" })
      .then((r) => {
        if (cancelled) return;
        const found = { files: r.files, truncated: r.truncated, at: Date.now() };
        agentFiles.set(profile.profileId, found);
        setCached(found);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(describeError(e, profile.name));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [session, profile.name, profile.profileId, online, allowed, roots, refresh, enabled]);
  return {
    cached,
    loading,
    error,
    allowed,
    refresh: () => setRefresh((n) => n + 1),
  };
}
export function useFile(rootId: string, path: string, enabled = true) {
  const { session, profile } = useApp(),
    link = useLink(),
    store = useLibrary();
  const cached = useStore(store, (s) => s.files[fileKey(rootId, path)]);
  const [error, setError] = useState<string | null>(null),
    [loading, setLoading] = useState(false),
    [refresh, setRefresh] = useState(0);
  const latest = useHost((s) => s.libraryRevisions[libraryKey(rootId, path)]);
  const online = link.state === "online",
    allowed = useMay("library_read");
  // A live revision change must never replace the base the reader opened.
  useEffect(() => {
    if (!online || !enabled || !allowed) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    void session
      .query({ kind: "library_read", root_id: rootId, path })
      .then((r) => {
        if (!cancelled) store.cacheFile(r.file);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(describeError(e, profile.name));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [session, profile.name, rootId, path, online, enabled, allowed, store, refresh]);
  return {
    cached,
    loading,
    error,
    allowed,
    latest,
    refresh: () => setRefresh((n) => n + 1),
  };
}
export function ConnectionNote({ at }: { at?: number }) {
  const link = useLink();
  if (link.state === "online") return null;
  return (
    <Notice
      tone={link.state === "failed" ? "stop" : "info"}
      title={
        link.state === "failed"
          ? "Computer access unavailable"
          : "Working from this device"
      }
    >
      {link.state === "failed"
        ? link.detail
        : "Connect to the computer to fetch files or save changes."}
      {at != null && (
        <>
          {" "}
          Last checked <Age at={at} suffix=" ago" />.
        </>
      )}
    </Notice>
  );
}
export function StorageNote() {
  const store = useLibrary();
  const error = useStore(store, (s) => s.storageError);
  const recovery = useStore(store, (s) => s.storageRecovery);
  return error ? (
    <Notice tone="stop" title="Draft storage needs attention">
      {error}
    </Notice>
  ) : recovery ? (
    <Notice tone="warn">{recovery}</Notice>
  ) : null;
}
export function LoadError({
  error,
  retry,
}: {
  error: string;
  retry: () => void;
}) {
  return (
    <Notice
      tone="stop"
      title="Couldn’t read from the computer"
      action={
        <Button icon={RefreshCw} onClick={retry}>
          Try again
        </Button>
      }
    >
      {error}
    </Notice>
  );
}
export function Filters({
  value,
  onChange,
}: {
  value: Filter;
  onChange: (v: Filter) => void;
}) {
  return (
    <div className="lib-filters" role="group" aria-label="File types">
      {filters.map((f) => (
        <Chip key={f} selected={value === f} onClick={() => onChange(f)}>
          {f}
        </Chip>
      ))}
    </div>
  );
}
export function Breadcrumbs({
  rootId,
  label,
  path,
  onChoose,
}: {
  rootId: string;
  label: string;
  path: string;
  onChoose?: (path: string) => void;
}) {
  const { ui } = useApp();
  const [open, setOpen] = useState(false);
  const { visible, collapsed } = breadcrumbs(label, path);
  const choose = (p: string) => {
    setOpen(false);
    if (onChoose) onChoose(p);
    else ui.up({ screen: "folder", rootId, path: p });
  };
  return (
    <>
      <nav className="lib-crumbs" aria-label="Folder path">
        {visible.map((c, i) => (
          <span key={c.path} className="lib-crumb">
            <Button variant="quiet" onClick={() => choose(c.path)}>
              {c.label}
            </Button>
            {i === 0 && path && <span aria-hidden>›</span>}
            {i === 0 && collapsed.length > 0 && (
              <>
                <Button
                  variant="quiet"
                  icon={MoreHorizontal}
                  aria-label="Parent folders"
                  onClick={() => setOpen(true)}
                />
                <span aria-hidden>›</span>
              </>
            )}
          </span>
        ))}
      </nav>
      <Sheet open={open} onClose={() => setOpen(false)} title="Parent folders" className="library-sheet">
        {collapsed.map((c) => (
          <ListRow
            key={c.path}
            icon={FolderIcon}
            title={c.label}
            subtitle={c.path}
            onClick={() => choose(c.path)}
          />
        ))}
      </Sheet>
    </>
  );
}
export function Diff({ base, text }: { base: string; text: string }) {
  const [expanded, setExpanded] = useState(false);
  const lines = useMemo(() => lineDiff(base, text), [base, text]);
  // Unchanged lines more than two away from a change collapse into one gap.
  const rows = useMemo(() => {
    const out: DiffRow[] = [];
    lines.forEach((l, i) => {
      const shown =
        expanded ||
        l.kind !== "same" ||
        lines.slice(Math.max(0, i - 2), i + 3).some((x) => x.kind !== "same");
      if (shown) out.push(l);
      else if (out.at(-1)?.kind !== "gap") out.push({ kind: "gap", text: "" });
    });
    return out;
  }, [lines, expanded]);
  const added = lines.filter((l) => l.kind === "add").length,
    removed = lines.filter((l) => l.kind === "remove").length;
  if (added + removed === 0)
    return (
      <Empty title="No line changes">
        Line endings are preserved when saving.
      </Empty>
    );
  return (
    <div className="lib-diff-wrap">
      <p className="lib-meta">
        + {added} added · − {removed} removed
      </p>
      <DiffLines rows={rows} label="Line changes" />
      {rows.some((r) => r.kind === "gap") && (
        <Button variant="quiet" onClick={() => setExpanded(true)}>
          Show unchanged lines
        </Button>
      )}
    </div>
  );
}
