// SPDX-License-Identifier: Apache-2.0
import { useEffect, useMemo } from "react";
import { AnnotationStore } from "../state/annotations";
import { useStore } from "../state/store";
import { Speech } from "../screens/annotate/Speech";
import { MAX_REPORT_WORDS } from "../diagnostics/constants";

export function ReportSpeech({ words, onWords, onRecording }: { words: string; onWords(words: string): void; onRecording(active: boolean): void }) {
  const voice = useMemo(() => { const store = new AnnotationStore("report-voice", false); return { store, id: store.create("", null).id }; }, []);
  const draft = useStore(voice.store, s => s[voice.id]!);
  const text = [draft.generalNote, ...draft.comments.map(c => c.text)].filter(Boolean).join("\n");
  useEffect(() => { if (text !== words) onWords(text); }, [text]); // Speech revisions update the same editable field.
  const edit = (value: string) => {
    voice.store.update(voice.id, d => ({ ...d, generalNote: value, comments: [], deletedSpeechSegments: d.speech?.segments.map(s => s.segment_id) ?? [] }));
    onWords(value);
  };
  return <div className="report-words">
    <label className="field"><span className="field__label">What happened?</span><textarea aria-label="What happened?" rows={3} value={words} maxLength={MAX_REPORT_WORDS} onChange={e => edit(e.target.value)} placeholder="A sentence is enough. You can leave this blank." /></label>
    <Speech keepAudio={false} draft={draft} store={voice.store} recordingTarget={null} onRecording={onRecording} />
  </div>;
}
