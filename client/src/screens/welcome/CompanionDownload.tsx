// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { PRODUCT_NAME } from "../../brand";
import type { Platform } from "../../host/link";
import { Button } from "../../design/Button";
import { companionDownload, shareCompanionSteps } from "./companionDownload";

export function CompanionDownload({ platform }: { platform: Platform }) {
  const config = companionDownload();
  const [status, setStatus] = useState<string | null>(null);
  const act = async (action: () => Promise<void>) => {
    try { await action(); } catch (e) {
      if (e instanceof Error && e.name === "AbortError") return;
      setStatus(e instanceof Error ? e.message : "Could not open the sharing or download action. Try again.");
    }
  };
  return <div className="welcome-download">
    {config.url ? <Button block onClick={() => void act(() => platform.openExternal(config.url!))}>Open download page</Button>
      : <p className="fine">This build has no download link for {PRODUCT_NAME}. Ask the person who supplied the app for the archive.</p>}
    {config.url && config.checksum && <Button block onClick={() => void act(() => platform.openExternal(config.checksum!))}>Check the archive checksum</Button>}
    <Button block onClick={() => void act(async () => {
      const result = await shareCompanionSteps(platform);
      setStatus(result === "copied" ? "Steps copied. Paste them into a message to your computer." : "Steps opened in the share sheet.");
    })}>Send these steps to your computer</Button>
    {status && <p className="fine" role="status">{status}</p>}
  </div>;
}
