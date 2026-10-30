// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef } from "react";
import { Redo2, Undo2 } from "lucide-react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, drawSelection, placeholder } from "@codemirror/view";
import {
  history,
  historyKeymap,
  defaultKeymap,
  undo,
  redo,
} from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { Button } from "../../design/Button";

/** Created only inside the lazy file screen. Never rebuild the document during IME composition. */
export default function Editor({
  initialText,
  empty = "Write in Markdown. Nothing reaches the computer until you review and save.",
  onChange,
  onComposing,
}: {
  initialText: string;
  /** Shown in an empty document, as a start. */
  empty?: string;
  onChange: (text: string) => void;
  onComposing: (v: boolean) => void;
}) {
  const parent = useRef<HTMLDivElement>(null),
    view = useRef<EditorView | null>(null),
    change = useRef(onChange),
    composing = useRef(onComposing);
  change.current = onChange;
  composing.current = onComposing;
  useEffect(() => {
    if (!parent.current) return;
    const editor = new EditorView({
      parent: parent.current,
      state: EditorState.create({
        doc: initialText,
        extensions: [
          markdown(),
          history(),
          drawSelection(),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          EditorView.lineWrapping,
          placeholder(empty),
          EditorView.contentAttributes.of({
            "aria-label": "Markdown source",
            spellcheck: "true",
            autocapitalize: "sentences",
          }),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) change.current(update.state.doc.toString());
          }),
          EditorView.domEventHandlers({
            compositionstart: () => {
              composing.current(true);
            },
            compositionend: () => {
              composing.current(false);
            },
          }),
          EditorView.theme({
            "&": {
              backgroundColor: "var(--surface-raised)",
              color: "var(--ink)",
              fontSize: "var(--text-md)",
            },
            ".cm-content": {
              fontFamily: "var(--font-mono)",
              padding: "var(--s4)",
              lineHeight: "1.65",
              minHeight: "12rem",
              caretColor: "var(--accent)",
            },
            ".cm-scroller": { overflow: "auto" },
            ".cm-line": { padding: "0", overflowWrap: "anywhere" },
            "&.cm-focused": { outline: "none" },
            "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
              backgroundColor: "var(--accent-soft)",
            },
            ".cm-cursor": { borderLeftColor: "var(--ink)" },
            ".cm-placeholder": { color: "var(--ink-2)", fontFamily: "var(--font-ui)" },
          }),
        ],
      }),
    });
    view.current = editor;
    return () => {
      editor.destroy();
      view.current = null;
    };
    // initialText is a mount-time value. Updates are already owned by CodeMirror.
  }, []);
  return (
    <div className="lib-editor">
      <div className="lib-toolbar">
        <Button
          variant="quiet"
          icon={Undo2}
          onClick={() => {
            if (view.current) {
              undo(view.current);
              view.current.focus();
            }
          }}
        >
          Undo
        </Button>
        <Button
          variant="quiet"
          icon={Redo2}
          onClick={() => {
            if (view.current) {
              redo(view.current);
              view.current.focus();
            }
          }}
        >
          Redo
        </Button>
      </div>
      <div ref={parent} />
    </div>
  );
}
