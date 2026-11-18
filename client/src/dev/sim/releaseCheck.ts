// SPDX-License-Identifier: Apache-2.0
import type { ReleaseCheckView } from "../../protocol/types";

/** Editable from window.__sim.host.releaseCheck for release-card drills. */
export class ReleaseCheckSim {
  view: ReleaseCheckView = {
    enabled: true, revision: 1, installed: "0.2.0", checked_at: Date.now() - 40 * 60_000,
    newer: null, checking: false, how_to_update: "Run ophio update on the computer to download the new version.",
  };
  set(enabled: boolean, revision: number | null) {
    if (revision !== this.view.revision) throw { code: "stale_revision", message: "Version checks changed. Refresh before changing them." };
    this.view = { ...this.view, enabled, revision: this.view.revision + 1, newer: enabled ? this.view.newer : null };
  }
}
