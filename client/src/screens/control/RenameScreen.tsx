// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { useApp, useHost } from "../../app/context";
import { SCREEN_NAME_MAX, connector, numberedName, renameScreen, screenName } from "../../app/screenNames";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { deviceKind } from "../../design/formFactor";
import { Sheet } from "../../design/Sheet";
import type { ControlTarget } from "../../protocol/types";

/** Names a screen on this device only; the computer keeps its connector name. */
export function RenameScreen({ target, onClose }: { target: ControlTarget; onClose(): void }) {
  const { profile, session } = useApp();
  const numbered = numberedName(useHost((s) => s.targets), target);
  const saved = screenName(profile.profileId, target);
  const [name, setName] = useState(target.label);
  const save = (value: string) => {
    renameScreen(profile.profileId, target, value);
    session.state.renamed();
    announce(`Screen named ${value.trim() || numbered}`);
    onClose();
  };
  return (
    <Sheet
      open
      title={`Rename ${target.label}`}
      onClose={onClose}
      footer={
        <Button variant="primary" disabled={!name.trim() || name.trim() === target.label} onClick={() => save(name)}>
          Save name
        </Button>
      }
    >
      <TextField
        label="Screen name"
        value={name}
        onChange={setName}
        maxLength={SCREEN_NAME_MAX}
        autoComplete="off"
        hint={`Only this ${deviceKind()} uses this name. The computer calls this screen ${connector(target)}.`}
      />
      {saved && <Button variant="quiet" onClick={() => save("")}>Go back to {numbered}</Button>}
    </Sheet>
  );
}
