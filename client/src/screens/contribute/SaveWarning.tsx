// SPDX-License-Identifier: Apache-2.0
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { useStore } from "../../state/store";
import { handoffs } from "./handoffs";

/** Shared by Tasks and both handoff screens, including after navigation. */
export function SaveWarning() {
  const store = handoffs();
  const failed = useStore(store.persistenceFailed, (s) => s);
  if (!failed) return null;
  return (
    <Notice tone="warn" title="Recovery details aren’t saved"
      action={<Button size="sm" onClick={() => store.retrySave()}>Try saving again</Button>}>
      This phone couldn’t save the handoffs. Free some storage, then try saving again. Keep Øphio open until they’re saved. A new download can start only after its recovery details are saved.
    </Notice>
  );
}
