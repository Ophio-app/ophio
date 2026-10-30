// SPDX-License-Identifier: Apache-2.0
import { Mic } from "lucide-react";
import { useApp } from "../../app/context";
import { ListRow, Section } from "../../design/ListRow";
import "./voice.css";
export function VoiceEntry() {
  const { ui } = useApp();
  return <Section title="Voice"><ListRow icon={Mic} title="Control by voice" subtitle="Speak or type a command for a browser or this desktop" chevron onClick={() => ui.push({ screen: "voice" })} /></Section>;
}
