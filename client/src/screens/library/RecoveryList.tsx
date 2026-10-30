// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { CircleHelp } from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { LibraryRecovery, LibraryRecoveryItem } from "../../protocol/types";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { useRoute } from "../../shell/Screen";

export function recoveryReason(recovery: LibraryRecovery): string {
  const { base, displaced } = recovery;
  if (base.state === "text" && displaced.state === "text" && base.file.revision !== displaced.file.revision) {
    return "The file changed on the computer while it was being saved";
  }
  if (displaced.state === "unavailable") return displaced.reason;
  return "The computer could not confirm this save";
}

export function RecoveryList() {
  const { session, profile, ui } = useApp(),
    { active } = useRoute(),
    online = useLink().state === "online";
  const canRead = useHost((s) => s.grants?.library_read),
    roots = useHost((s) => s.libraryRoots);
  const [items, setItems] = useState<
      (LibraryRecoveryItem & { rootPath: string | undefined; reason?: string })[] | null
    >(null),
    [at, setAt] = useState<number>(),
    [error, setError] = useState<string | null>(null),
    [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!online || !canRead || !active) return;
    let alive = true;
    void session
      .query({ kind: "library_recovery_list" })
      .then((r) => {
        if (alive) {
          setItems(
            r.items.map((i) => ({
              ...i,
              rootPath: roots.find((root) => root.root_id === i.root_id)
                ?.host_path,
            })),
          );
          setAt(Date.now());
          setError(null);
          // Read each retained snapshot through the existing authorized query.
          // Failure to inspect one copy must not hide the recovery list.
          for (const item of r.items) {
            void session.query({ kind: "library_recovery_read", root_id: item.root_id, recovery_id: item.recovery_id })
              .then(({ recovery }) => {
                if (alive) setItems(items => items?.map(i => i.recovery_id === item.recovery_id
                  ? { ...i, reason: recoveryReason(recovery) } : i) ?? null);
              }).catch(() => { /* The recovery screen offers retry and the read error. */ });
          }
        }
      })
      .catch((e: unknown) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [session, profile.name, online, canRead, active, roots, retry]);
  if (!canRead) return null;
  const content = (
    <Section title={items?.length ? "Needs checking" : items ? "Recovery copies · none" : "Recovery copies"}>
      <div className="lib-stack">
        {error ? (
          <Notice
            tone="unknown"
            action={
              <Button disabled={!online} onClick={() => setRetry((n) => n + 1)}>
                Check recovery copies
              </Button>
            }
          >
            {error}
          </Notice>
        ) : items === null ? (
          <p className="lib-meta">
            {online
              ? "Checking for retained saves…"
              : "Connect to check for retained saves."}
          </p>
        ) : (
          <>
            {items.length === 0 && (
              <p className="lib-meta">
                No retained saves
                {at != null && (
                  <>
                    {" "}
                    · Checked <Age at={at} suffix=" ago" />
                  </>
                )}
              </p>
            )}
            {items
              .filter((i) =>
                roots.some(
                  (r) => r.root_id === i.root_id && r.host_path === i.rootPath,
                ),
              )
              .map((i) => (
                <ListRow
                  key={i.recovery_id}
                  icon={CircleHelp}
                  title={i.path}
                  subtitle={
                    <>
                      {i.reason ?? "Retained save"} · Recovery kept{" "}
                      <Age at={i.created_at} suffix=" ago" />
                    </>
                  }
                  onClick={() =>
                    ui.push({
                      screen: "library_recovery",
                      rootId: i.root_id,
                      recoveryId: i.recovery_id,
                    })
                  }
                  chevron
                />
              ))}
          </>
        )}
      </div>
    </Section>
  );
  return items?.length || error ? content : (
    <details className="lib-recovery-status">
      <summary>Recovery copies</summary>
      {content}
    </details>
  );
}
