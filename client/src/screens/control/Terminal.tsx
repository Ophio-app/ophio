// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { terminalTitle } from "./terminalTitle";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ArrowDown, ArrowUp, Copy, Keyboard, KeyboardOff, Minus, Plus, LogIn, LogOut, X } from "lucide-react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import {
  useApp,
  useCommand,
  useHost,
  useLink,
  describeError,
} from "../../app/context";
import { commandResult } from "../../host/commandResult";
import type { TerminalLink } from "../../host/link";
import { Age } from "../../design/Age";
import { Button, IconButton } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { TextArea } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { toast } from "../../design/Toast";
import { announce } from "../../design/Announcer";
import { Screen } from "../../shell/Screen";
import { hideKeyboard, useCrampedLayout, useKeyboardOpen, useSidewaysLayout, useTightLayout } from "../../shell/keyboard";
import type { RouteProps } from "../types";
import { useLocalText } from "./local";
import { useVisible } from "./useControl";
import { ConnectionNote } from "./Feedback";
import { terminalKey } from "./keys";
import { keyCap } from "./KeyCap";
import { useStore } from "../../state/store";
import { TerminalOffset, cleanPaste, pasteLines } from "./terminalBuffer";
import { touchScroll } from "./touchScroll";
import { openMeasuringText, remeasureOnTextZoom } from "./terminalMeasure";
import { terminalHistory, useTerminalEnds } from "./terminalHistory";
import { clockTime } from "../../design/time";
import "./control.css";
import { useWarning } from "../../app/warnings";

type Attachment =
  "attaching" | "attached" | "offline" | "detached" | "exited" | "gone" | "error";
export default function Terminal({ route }: RouteProps<"terminal">) {
  const { platform, profile, session, ui } = useApp();
  const online = useLink().state === "online";
  const visible = useVisible();
  // Short for the text size, the field's name moves inside it and Send
  // shrinks to its icon, so the line fits beside it.
  const tight = useTightLayout();
  const keyboardOpen = useKeyboardOpen();
  const cramped = useCrampedLayout();
  const sideways = useSidewaysLayout();
  const info = useHost((s) =>
    s.terminals.find((t) => t.terminal_id === route.terminalId),
  );
  // Retain the launch context after removing a refused entry from Control home.
  const remembered = useRef(info);
  if (info) remembered.current = info;
  const [gone, setGone] = useState(false);
  const [creating, setCreating] = useState(false);
  const grant = useHost((s) => s.grants?.terminal);
  const command = useCommand();
  const closeCommand = useStore(session.commands, (all) =>
    Object.values(all)
      .filter(
        (c) =>
          c.body.command === "close_terminal" &&
          c.body.terminal_id === route.terminalId,
      )
      .at(-1),
  );
  const [draft, setDraft] = useLocalText(
    `terminal.draft.${profile.profileId}.${route.terminalId}`,
  );
  const [font, setFont] = useLocalText("terminal.font-size", "14");
  const fontSize = Math.max(12, Math.min(24, Number(font) || 14));
  const [mode, setMode] = useState<"text" | "keys">("text");
  const [wanted, setWanted] = useState(true);
  const [attempt, setAttempt] = useState(0);
  const ended = useRef(info?.state.state === "exited");
  const [attachment, setAttachment] = useState<Attachment>(ended.current ? "exited" : "attaching");
  const [error, setError] = useState<string | null>(null);
  const [outputAt, setOutputAt] = useState<number | null>(null);
  const [selection, setSelection] = useState(false);
  const [scrolled, setScrolled] = useState(false);
  const [close, setClose] = useState(false);
  const [closing, setClosing] = useState(false);
  const closeRequested = useRef(false);
  const terminalEnd = useTerminalEnds()[route.terminalId];
  const [paste, setPaste] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const commandField = useRef<HTMLTextAreaElement>(null);
  const [textFocused, setTextFocused] = useState(false);
  const typing = tight && mode === "text" && textFocused;
  // Typing, cramped for the text size, or on a phone on its side, the output
  // gets the height: while attached the status line gives way, the command
  // keeps to one row, and on a phone on its side the bar and the input share
  // the top row.
  const compact = typing || cramped || sideways;
  const terminal = useRef<XTerm | null>(null);
  const refit = useRef<() => void>(() => {});
  const link = useRef<TerminalLink | null>(null);
  const offset = useRef(new TerminalOffset());
  const composing = useRef(false);
  const inputReady =
    visible &&
    online &&
    grant === true &&
    wanted &&
    attachment === "attached" &&
    closeCommand?.phase !== "sending" &&
    closeCommand?.phase !== "unknown" &&
    closeCommand?.phase !== "accepted" &&
    info?.state.state !== "exited";
  const current = useRef({ inputReady, mode });
  current.current = { inputReady, mode };
  const named = useHost(s => info ? terminalTitle(info, s.terminals) : null);
  const rememberedTitle = useRef(named);
  if (named) rememberedTitle.current = named;
  const title = named ?? rememberedTitle.current ?? "Terminal";
  const sessionEnded = gone || attachment === "exited" || info?.state.state === "exited";
  const transmit = (text: string) => {
    if (
      !current.current.inputReady ||
      !link.current?.input(new TextEncoder().encode(text))
    ) {
      toast("Input was not sent. Reconnect to this terminal first.", {
        tone: "stop",
      });
      return false;
    }
    terminal.current?.scrollToBottom();
    return true;
  };
  const transmitRef = useRef(transmit);
  transmitRef.current = transmit;
  const closeWarning = useWarning("terminal.close");
  const pasteWarning = useWarning("terminal.paste");
  const pasteNow = (raw: string) => {
    const text = cleanPaste(raw);
    if (current.current.mode === "text") {
      if (transmit(text.replaceAll("\n", "\r") + "\r")) setDraft("");
    } else terminal.current?.paste(text);
  };
  const askPaste = useRef((_text: string) => {});
  askPaste.current = (text: string) => pasteWarning.ask(() => setPaste(text), () => pasteNow(text));
  const askClose = () => closeWarning.ask(() => setClose(true), () => void closeTerminal());
  const closeTerminal = async () => {
    setClosing(true);
    closeRequested.current = true;
    try {
      const r = await command(
        { command: "close_terminal", terminal_id: route.terminalId },
        { label: `Close ${title}` },
      );
      if (!r) return;
      await commandResult(session, r);
      terminalHistory(session).closed(route.terminalId, session.state.get().commandStages[r.command_id]?.at ?? Date.now());
      setClose(false);
    } catch (reason) {
      setClose(false);
      setError(describeError(reason, profile.name));
    } finally {
      setClosing(false);
    }
  };

  useEffect(() => {
    if (info?.state.state === "exited") {
      ended.current = true;
      setError(terminalEnd?.closedAt !== undefined ? null : info.state.code === null ? "This terminal has exited. Its exit code is unknown." : `This terminal exited with code ${info.state.code}.`);
      setAttachment("exited");
      current.current.inputReady = false;
    }
  }, [info?.state, terminalEnd?.closedAt]);

  useEffect(() => {
    if (visible && closeRequested.current && terminalEnd?.closedAt !== undefined) {
      closeRequested.current = false;
      setClose(false);
      ui.reset("control");
      requestAnimationFrame(() => document.getElementById("control-terminals")?.scrollIntoView({ block: "start" }));
    }
  }, [visible, terminalEnd?.closedAt, ui]);

  useEffect(() => {
    if (!box.current) return;
    const term = new XTerm({
      cursorBlink: false,
      convertEol: false,
      fontSize: 14,
      fontFamily: "JetBrains Mono Variable, monospace",
      // What can be swiped back through: enough for a long build or test log.
      // xterm keeps 12 bytes a cell, under 5 MB at 80 columns, and rewraps it
      // all whenever the phone turns or the keyboard opens, which stays quick
      // at this size.
      scrollback: 5000,
      allowProposedApi: false,
      disableStdin: true,
      screenReaderMode: true,
    });
    const fitter = new FitAddon();
    term.loadAddon(fitter);
    openMeasuringText(term, box.current);
    terminal.current = term;

    const theme = () => {
      const css = getComputedStyle(document.documentElement),
        token = (name: string) => css.getPropertyValue(name).trim();
      term.options.theme = {
        background: token("--surface-sunken"),
        foreground: token("--ink"),
        cursor: token("--accent"),
        selectionBackground: token("--accent-soft"),
        black: token("--ink"),
        red: token("--stop-text"),
        green: token("--ok-text"),
        yellow: token("--warn-text"),
        blue: token("--accent-text"),
        magenta: token("--mark-2"),
        cyan: token("--mark-5"),
        white: token("--ink-2"),
        brightBlack: token("--ink-3"),
        brightRed: token("--stop-text"),
        brightGreen: token("--ok-text"),
        brightYellow: token("--warn-text"),
        brightBlue: token("--accent-text"),
        brightMagenta: token("--mark-2"),
        brightCyan: token("--mark-5"),
        brightWhite: token("--ink"),
      };
    };
    theme();
    const observer = new MutationObserver(theme);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    // Remote escape sequences never gain access to the phone clipboard.
    const clipboard = term.parser.registerOscHandler(52, () => true);
    const data = term.onData((text) => {
      if (current.current.mode === "keys" && current.current.inputReady)
        transmitRef.current(text);
    });
    const selected = term.onSelectionChange(() =>
      setSelection(term.hasSelection()),
    );
    const scroll = term.onScroll(() =>
      setScrolled(term.buffer.active.viewportY < term.buffer.active.baseY),
    );
    const swipe = touchScroll(box.current, {
      position: () => ({ top: term.buffer.active.viewportY, latest: term.buffer.active.baseY }),
      scrollLines: (lines) => term.scrollLines(lines),
      rowHeight: () => (term.element?.querySelector<HTMLElement>(".xterm-screen")?.offsetHeight ?? 0) / term.rows,
      glide: () => document.documentElement.dataset.reduceMotion !== "true",
    });
    let frame = 0;
    let disposed = false;
    const fitVisible = () => {
      if (disposed || !box.current || box.current.clientWidth <= 0 || box.current.clientHeight <= 0) return;
      const latest = term.buffer.active.viewportY >= term.buffer.active.baseY;
      fitter.fit();
      if (latest) term.scrollToBottom();
      if (current.current.inputReady) link.current?.send({ type: "resize", cols: term.cols, rows: term.rows });
    };
    const scheduleFit = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(fitVisible);
    };
    refit.current = scheduleFit;
    const rezoomed = remeasureOnTextZoom(term, scheduleFit);
    const resize = new ResizeObserver(scheduleFit);
    window.visualViewport?.addEventListener("resize", scheduleFit);
    window.addEventListener("resize", scheduleFit);
    void document.fonts?.ready.then(() => { if (!disposed) scheduleFit(); });
    resize.observe(box.current);
    const pasted = (e: ClipboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!current.current.inputReady || current.current.mode !== "keys")
        return;
      const text = cleanPaste(e.clipboardData?.getData("text/plain") ?? "");
      if (text.includes("\n")) askPaste.current(text);
      else if (text) term.paste(text);
    };
    box.current.addEventListener("paste", pasted, true);
    const element = box.current;
    return () => {
      element.removeEventListener("paste", pasted, true);
      disposed = true;
      cancelAnimationFrame(frame);
      window.visualViewport?.removeEventListener("resize", scheduleFit);
      window.removeEventListener("resize", scheduleFit);
      resize.disconnect();
      rezoomed();
      observer.disconnect();
      clipboard.dispose();
      data.dispose();
      selected.dispose();
      scroll.dispose();
      swipe();
      term.dispose();
      terminal.current = null;
      refit.current = () => {};
    };
  }, []);

  useEffect(() => {
    const term = terminal.current;
    if (!term) return;
    // Page text: the phone's text size zooms it like the rest of the app.
    term.options.fontSize = fontSize;
    term.options.disableStdin = !inputReady || mode !== "keys";
    // xterm keeps its readonly input sink even after exit. Retain accessible
    // output, but remove the ended session's input from focus and the tree.
    const input = term.textarea;
    if (input) {
      const finished = attachment === "exited" || gone;
      input.hidden = finished;
      input.disabled = finished;
      input.tabIndex = finished ? -1 : 0;
      if (finished) input.setAttribute("aria-hidden", "true");
      else input.removeAttribute("aria-hidden");
    }
    if (visible && box.current && box.current.clientWidth > 0) {
      refit.current();
    }
  }, [fontSize, inputReady, mode, visible, attachment, gone]);

  useEffect(() => {
    if (gone) { setAttachment("gone"); return; }
    if (!visible || !online || !wanted || !grant) {
      setAttachment(
        ended.current
          ? "exited"
          : wanted
            ? "offline"
            : "detached",
      );
      return;
    }
    const terminalLink = platform.terminal(profile.profileId);
    link.current = terminalLink;
    let disposed = false,
      sent = false;
    let attached = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (!ended.current) {
      setAttachment("attaching");
      setError(null);
    }
    const attach = () => {
      if (!sent && !disposed) {
        sent = terminalLink.send({
          type: "attach",
          terminal_id: route.terminalId,
          from_offset: offset.current.next,
        });
        clearTimeout(timer);
        timer = setTimeout(() => {
          if (!disposed && !attached && !ended.current) {
            setError(
              "The computer hasn’t confirmed attachment to this terminal.",
            );
            setAttachment("error");
          }
        }, 10_000);
      }
    };
    const stop = terminalLink.listen((e) => {
      if (disposed) return;
      if (e.type === "state") {
        if (e.state.state === "online") {
          if (!sent) {
            if (!ended.current) setAttachment("attaching");
            attach();
          }
        } else {
          if (ended.current) return;
          sent = false;
          attached = false;
          clearTimeout(timer);
          current.current.inputReady = false;
          if (e.state.state === "offline" && e.state.code === "not_found") {
            setGone(true);
            setAttachment("gone");
            setError(null);
            session.state.set(s => ({ ...s, terminals: s.terminals.filter(t => t.terminal_id !== route.terminalId) }));
          } else if (e.state.state === "offline" || e.state.state === "failed") {
            setAttachment("offline");
            const reason = e.state.state === "offline" ? e.state.reason : e.state.detail;
            setError(reason && /^(connection failed|connection interrupted|offline)$/i.test(reason.trim())
              ? "The terminal connection was interrupted. Reconnect to try again." : reason);
          } else {
            setAttachment("attaching");
          }
        }
      } else if (e.type === "text") {
        const f = e.frame;
        if (f.type === "attached") {
          attached = true;
          clearTimeout(timer);
          if (f.terminal.terminal_id !== route.terminalId) {
            setError(
              "The computer attached a different terminal. Input is disabled.",
            );
            setAttachment("error");
            return;
          }
          try {
            offset.current.attach(f.replay_from, f.replay_truncated);
            if (f.replay_truncated) {
              terminal.current?.reset();
              setError(
                "Earlier output is no longer retained on the computer. Showing the available scrollback.",
              );
            }
            session.state.set((s) => ({
              ...s,
              terminals: [
                ...s.terminals.filter(
                  (t) => t.terminal_id !== f.terminal.terminal_id,
                ),
                f.terminal,
              ],
            }));
            ended.current = f.terminal.state.state === "exited";
            setAttachment(ended.current ? "exited" : "attached");
            const term = terminal.current;
            if (term && !ended.current)
              terminalLink.send({
                type: "resize",
                cols: term.cols,
                rows: term.rows,
              });
            announce(`Attached to ${f.terminal.title}`);
          } catch (reason) {
            setError(describeError(reason, profile.name));
            setAttachment("error");
          }
        } else if (f.type === "error") {
          if (ended.current) return;
          setError(f.message);
          setAttachment("error");
          current.current.inputReady = false;
        } else if (f.type === "exited") {
          ended.current = true;
          clearTimeout(timer);
          session.state.set(s => ({ ...s, terminals: s.terminals.map(t => t.terminal_id === route.terminalId ? { ...t, state: { state: "exited", code: f.code } } : t) }));
          terminalLink.close();
          setAttachment("exited");
          setError(
            f.code === null
              ? "This terminal has exited. Its exit code is unknown."
              : `This terminal exited with code ${f.code}.`,
          );
          current.current.inputReady = false;
        }
      } else {
        try {
          const bytes = offset.current.accept(e.data);
          if (bytes.length) {
            const term = terminal.current;
            const latest = term && term.buffer.active.viewportY >= term.buffer.active.baseY;
            term?.write(bytes, () => { if (latest) term.scrollToBottom(); });
            setOutputAt(Date.now());
          }
        } catch (reason) {
          setError(describeError(reason, profile.name));
          setAttachment("error");
          current.current.inputReady = false;
        }
      }
    });
    attach();
    return () => {
      disposed = true;
      current.current.inputReady = false;
      clearTimeout(timer);
      stop();
      if (!ended.current) terminalLink.send({ type: "detach" });
      terminalLink.close();
      link.current = null;
    };
  }, [
    platform,
    profile.profileId,
    profile.name,
    route.terminalId,
    session,
    visible,
    online,
    wanted,
    grant,
    attempt,
    gone,
  ]);

  const sendLine = () => {
    if (composing.current || !inputReady || !draft) return;
    if (draft.includes("\n")) {
      askPaste.current(draft);
      return;
    }
    if (transmit(draft + "\r")) { setDraft(""); commandField.current?.focus({ preventScroll: true }); }
  };
  const detach = () => {
    current.current.inputReady = false;
    setWanted(false);
  };
  const closable = online && !!info && info.state.state !== "exited";
  // Short for the text size the output fills the screen, so the session's
  // own controls move up into the bar rather than below the output.
  const barActions = tight && !sessionEnded && !typing ? <>
    {wanted
      ? <IconButton icon={LogOut} label="Detach" onClick={detach} />
      : <IconButton icon={LogIn} label="Attach again" variant="primary" disabled={!online} onClick={() => setWanted(true)} />}
    <IconButton icon={X} label="Close terminal" className="terminal-close" disabled={!closable || closing} onClick={askClose} />
  </> : undefined;
  const footer = (
    <div className="control-footer terminal-composer">
      {!typing && <TerminalModeBar mode={mode} onChange={setMode} inputReady={inputReady} short={compact} besideBar={sideways}
        showKeyboard={() => terminal.current?.focus()}>
        {["Esc", "Tab", "Ctrl+C", "Ctrl+Z", "←", "↑", "↓", "→", "Enter", "Backspace"].map(k => (
          <Button key={k} size="sm" disabled={!inputReady} {...keyCap(k)}
            onPointerDown={e => e.preventDefault()}
            onClick={() => { const bytes = terminalKey(k); if (bytes) transmit(bytes); }} />
        ))}
      </TerminalModeBar>}
      {closeCommand && info?.state.state !== "exited" && (
        <StateLine
          phase={
            closeCommand.phase === "unknown"
              ? "unknown"
              : closeCommand.phase === "sending"
                ? "sending"
                : closeCommand.phase === "rejected"
                  ? "rejected"
                  : closeCommand.phase === "not_delivered"
                    ? "not_delivered"
                    : "host_accepted"
          }
          detail={
            closeCommand.error?.message ??
            "Close requested. Waiting for the session’s exit."
          }
        />
      )}
      {/* Cramped, the status line above the output already says why. */}
      {!inputReady && !compact && (
        <p className="fine">
          {attachment === "detached"
            ? "Detached. Programs keep running on the computer."
            : attachment === "exited"
              ? "This session has ended."
              : "Input disabled until this terminal is attached and connected."}
        </p>
      )}
      {mode === "text" ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            sendLine();
          }}
        >
          <TextArea
            ref={commandField}
            onFocus={() => setTextFocused(true)}
            onBlur={() => setTextFocused(false)}
            label="Command"
            hideLabel={tight}
            placeholder={tight ? "Command" : undefined}
            aria-label={`Command for ${title}`}
            value={draft}
            onChange={setDraft}
            disabled={!inputReady}
            rows={1}
            maxRows={compact ? 1 : 3}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            onCompositionStart={() => {
              composing.current = true;
            }}
            onCompositionEnd={() => {
              composing.current = false;
            }}
            onKeyDown={(e) => {
              if (
                e.key === "Enter" &&
                !e.shiftKey &&
                !e.nativeEvent.isComposing &&
                !composing.current
              ) {
                e.preventDefault();
                sendLine();
              }
            }}
          />
          {tight ? (
            <IconButton
              onPointerDown={e => e.preventDefault()}
              type="submit"
              icon={ArrowUp}
              label="Send line"
              variant="primary"
              disabled={!inputReady || !draft}
            />
          ) : (
            <Button
              onPointerDown={e => e.preventDefault()}
              type="submit"
              variant="primary"
              disabled={!inputReady || !draft}
            >
              Send line
            </Button>
          )}
          {draft && !typing && <StateLine phase="saved_on_phone" />}
        </form>
      ) : null}
    </div>
  );
  return (
    <Screen
      title={title}
      subtitle={profile.name}
      className={`control-screen terminal-screen${compact ? " terminal-screen--compact" : ""}${typing ? " terminal-screen--typing" : ""}`}
      actions={barActions}
      footer={sessionEnded ? undefined : footer}
    >
      <div className="terminal-view">
        <ConnectionNote />
        {(!online && !gone && !ended.current) || (compact && attachment === "attached" && !error) ? undefined :
          <div className="terminal-recovery">
            <div className="control-row">
              <p className="terminal-status" role="status">
                {gone ? "Terminal ended" : attachment === "attached" ? "Attached"
                  : attachment === "attaching" ? "Attaching…" : attachment === "offline" ? "Connection interrupted"
                  : attachment === "detached" ? "Detached" : attachment === "exited"
                    ? terminalEnd?.closedAt !== undefined ? `Closed by you · ${clockTime(terminalEnd.closedAt)}`
                      : `Session ended${terminalEnd ? ` · Exit observed ${clockTime(terminalEnd.observedAt)}` : ""}`
                    : "Attachment unavailable"}
                {outputAt !== null && <> · Output <Age at={outputAt} suffix=" ago" /></>}
              </p>
              {gone ? <>
                <Button variant="primary" icon={Plus} pending={creating} disabled={!online || !grant} onClick={async () => {
                  setCreating(true); setError(null);
                  try {
                    const previous = remembered.current;
                    const result = await commandResult(session, await session.command({ command: "open_terminal",
                      project_id: previous?.project_id ?? null, cwd: previous?.cwd,
                      harness: null, cols: 80, rows: 24 }, { label: "Open a new terminal" }));
                    if (result?.kind !== "terminal_opened") throw new Error("The computer did not confirm the new terminal.");
                    ui.back();
                    ui.push({ screen: "terminal", terminalId: result.terminal.terminal_id });
                  } catch (reason) { setError(describeError(reason, profile.name)); }
                  finally { setCreating(false); }
                }}>{creating ? "Opening a terminal…" : "Open a new terminal"}</Button>
                <Button onClick={() => ui.back()}>Back</Button>
              </> : wanted && !["attached", "exited"].includes(attachment) &&
                <Button disabled={!online} pending={attachment === "attaching"} onClick={() => {
                  setAttachment("attaching"); setError(null); setAttempt(n => n + 1);
                }}>{attachment === "attaching" ? "Reconnecting…" : tight ? "Reconnect" : "Reconnect terminal"}</Button>}
            </div>
            {gone && <p>This terminal is no longer on {profile.name}. Terminals end when {PRODUCT_NAME} restarts on the computer.</p>}
            {error && <p className="error-text" role="status">{error}</p>}
          </div>
        }
        {!grant && (
          <Notice tone="warn">
            This device doesn’t have the Terminals permission. Allow it on the
            computer.
          </Notice>
        )}
        <div className="terminal-frame" hidden={gone}>
          <div
            className="terminal-output"
            ref={box}
            data-scroll-x
            aria-label={`${title} output`}
          />
          {scrolled && (
            <Button
              size="sm"
              icon={ArrowDown}
              className="terminal-jump"
              onClick={() => terminal.current?.scrollToBottom()}
            >
              Jump to latest
            </Button>
          )}
        </div>
      </div>
      <details className="terminal-options" hidden={gone || typing} open={!sessionEnded && !keyboardOpen}>
      <summary>Output options</summary>
      {!sessionEnded && <p className="fine">
        {mode === "text"
          ? "Compose a line, then send it with Enter. Shift+Enter adds a line to review."
          : "Keys go directly to the attached terminal."}
      </p>}
      <div className="terminal-toolbar">
        <IconButton
          icon={Minus}
          label="Decrease terminal font"
          disabled={fontSize <= 12}
          onClick={() => setFont(String(fontSize - 1))}
        />
        <span className="fine">{fontSize} px</span>
        <IconButton
          icon={Plus}
          label="Increase terminal font"
          disabled={fontSize >= 24}
          onClick={() => setFont(String(fontSize + 1))}
        />
        <Button
          icon={Copy}
          size="sm"
          disabled={!selection}
          onClick={() => {
            const selected = terminal.current?.getSelection();
            if (selected)
              void navigator.clipboard.writeText(selected).then(
                () => toast("Selection copied"),
                () =>
                  toast(
                    "Copy is unavailable. Use your device’s text selection menu.",
                    { tone: "stop" },
                  ),
              );
          }}
        >
          Copy selection
        </Button>
      </div>
      </details>
      {!sessionEnded && <div className="terminal-session" hidden={typing}>
        {info && <p className="control-path mono">{info.cwd}</p>}
        {!tight && <div className="control-keys">
          {wanted ? (
            <Button icon={LogOut} onClick={detach}>
              Detach
            </Button>
          ) : (
            <Button
              variant="primary"
              disabled={!online}
              onClick={() => setWanted(true)}
            >
              Attach again
            </Button>
          )}
          <Button
            icon={X}
            variant="stop-outline"
            disabled={!closable}
            pending={closing && !close}
            onClick={askClose}
          >
            Close terminal
          </Button>
        </div>}
        <p className="fine">
          Detach leaves programs running. Close terminal ends the session and its
          running programs.
        </p>
      </div>}
      <Sheet
        open={close && visible}
        onClose={() => setClose(false)}
        title={`Close ‘${title}’?`}
        closeLabel="Keep terminal"
        footer={
          <Button
            variant="stop"
            block
            pending={closing}
            disabled={!online}
            onClick={() => { closeWarning.agreed(); void closeTerminal(); }}
          >
            Close ‘{title}’
          </Button>
        }
      >
        <p>
          Running programs in ‘{title}’ will end. Their process state cannot be
          restored. Files already written stay on the computer.
        </p>
        <p className="mono control-path">{info?.cwd}</p>
        {closeWarning.check}
      </Sheet>
      <Sheet
        open={paste !== null && visible}
        onClose={() => setPaste(null)}
        title={`Paste ${pasteLines(paste ?? "")} into ‘${title}’?`}
        footer={
          <Button
            variant="primary"
            block
            disabled={!inputReady}
            onClick={() => {
              if (paste === null) return;
              pasteWarning.agreed();
              pasteNow(paste);
              setPaste(null);
            }}
          >
            Paste into ‘{title}’
          </Button>
        }
      >
        <p>
          These lines will be sent to the running program. They may execute
          commands.
        </p>
        <pre className="terminal-paste">{paste}</pre>
        {pasteWarning.check}
      </Sheet>
    </Screen>
  );
}

/** Stays above the keyboard with the input row, independently of scrollback. */
export function TerminalModeBar({ mode, onChange, inputReady, showKeyboard, short, besideBar, children }: {
  mode: "text" | "keys";
  onChange: (mode: "text" | "keys") => void;
  inputReady: boolean;
  showKeyboard: () => void;
  /** The switch shares a row with the command field or the keys. */
  short?: boolean;
  /** The switch shares a row with the screen's bar, which offers Hide keyboard itself. */
  besideBar?: boolean;
  children?: ReactNode;
}) {
  const keyboardOpen = useKeyboardOpen();
  const cramped = useCrampedLayout();
  const brief = short ?? cramped;
  return <div className="terminal-modebar">
    <Segmented label="Terminal input mode" value={mode}
      options={[{ value: "text", label: brief ? "Text" : "Text mode" }, { value: "keys", label: brief ? "Keys" : "Key mode" }]}
      onChange={value => { hideKeyboard(); onChange(value); }} />
    {mode === "keys" && <div className="control-keys terminal-keys" data-scroll-x>
      {/* Sharing a row, the toggle keeps to its icon so Ctrl+C stays in view. */}
      {besideBar && keyboardOpen ? null : brief
        ? <IconButton icon={keyboardOpen ? KeyboardOff : Keyboard} label={keyboardOpen ? "Hide keyboard" : "Show keyboard"}
          disabled={!inputReady && !keyboardOpen} onPointerDown={e => e.preventDefault()}
          onClick={() => keyboardOpen ? hideKeyboard() : showKeyboard()} />
        : <Button disabled={!inputReady && !keyboardOpen}
          onPointerDown={e => e.preventDefault()}
          onClick={() => keyboardOpen ? hideKeyboard() : showKeyboard()}>
          {keyboardOpen ? "Hide keyboard" : "Show keyboard"}
        </Button>}
      {children}
    </div>}
  </div>;
}
