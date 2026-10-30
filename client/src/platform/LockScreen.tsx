// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { LockKeyhole } from "lucide-react";
import { Button } from "../design/Button";
import { Notice } from "../design/Notice";
import { useStore } from "../state/store";
import type { LockController } from "./lock";
import "./lifecycle.css";

export function LockScreen({ controller }: { controller: LockController }) {
  const state = useStore(controller, (s) => s);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { heading.current?.focus(); }, []);
  useEffect(() => {
    if (state.locked && !state.checking && isTauri()) {
      // Runs after private React trees have unmounted; Android can remove its startup cover.
      void invoke("app_lock_screen_ready").catch(() => undefined);
    }
  }, [state]);
  return <main className="local-lock">
    <div className="local-lock__content">
      <LockKeyhole aria-hidden />
      <h1 ref={heading} tabIndex={-1}>{state.checking ? "Checking app lock…" : "App locked"}</h1>
      <p>Unlock with your device’s biometrics or screen lock.</p>
      {state.error && <Notice tone="warn">{state.error}</Notice>}
      <Button variant="primary" size="lg" pending={state.busy} onClick={() => void controller.unlock()}>Unlock app</Button>
      <p className="muted">Unlocking doesn’t approve requests or start work.</p>
    </div>
  </main>;
}
