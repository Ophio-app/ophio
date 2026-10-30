// SPDX-License-Identifier: Apache-2.0
//
// A conversation stays on its newest line until the person scrolls away. On a
// phone, continuing a finished task closes the keyboard as the new run's first
// lines arrive; the browser moves the view for the taller region, and a new
// line can arrive before the view has followed it. Those scrolls are the
// layout moving, and taking them for the person leaving showed Latest and let
// the new run grow out of sight below it. The scrolls here are the ones traced
// on the emulator when that happened.
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useStickToBottom } from "./stickToBottom";

/** The region's content and visible heights, and where it is scrolled to. */
let height = 0;
let view = 0;
let top = 0;
/** The window's height, which the keyboard takes from. */
let win = 0;
const innerHeight = Object.getOwnPropertyDescriptor(window, "innerHeight")!;
let resized: (() => void)[] = [];
let scrolledTo: ScrollToOptions[] = [];
let hook: ReturnType<typeof useStickToBottom>;

const own = (el: Element) => (el as HTMLElement).dataset?.region === "talk";
const inherited = (name: string) => Object.getOwnPropertyDescriptor(Element.prototype, name)!;

beforeEach(() => {
  height = 4304;
  view = 255;
  top = 0;
  win = 603;
  Object.defineProperty(window, "innerHeight", { configurable: true, get: () => win });
  resized = [];
  scrolledTo = [];
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(cb: ResizeObserverCallback) {
        resized.push(() => cb([], this as unknown as ResizeObserver));
      }
      observe() {}
      disconnect() {}
    },
  );
  for (const [name, value] of [["scrollHeight", () => height], ["clientHeight", () => view]] as const) {
    const base = inherited(name);
    Object.defineProperty(HTMLElement.prototype, name, {
      configurable: true,
      get(this: HTMLElement) {
        return own(this) ? value() : base.get!.call(this);
      },
    });
  }
  const base = inherited("scrollTop");
  Object.defineProperty(HTMLElement.prototype, "scrollTop", {
    configurable: true,
    get(this: HTMLElement) {
      return own(this) ? top : base.get!.call(this);
    },
    set(this: HTMLElement, v: number) {
      if (own(this)) top = Math.max(0, Math.min(v, height - view));
      else base.set!.call(this, v);
    },
  });
  HTMLElement.prototype.scrollTo = function (this: HTMLElement, o?: ScrollToOptions | number) {
    if (typeof o === "object") scrolledTo.push(o);
  } as HTMLElement["scrollTo"];
});

afterEach(() => {
  cleanup();
  for (const name of ["scrollHeight", "clientHeight", "scrollTop", "scrollTo"]) {
    delete (HTMLElement.prototype as unknown as Record<string, unknown>)[name];
  }
  Object.defineProperty(window, "innerHeight", innerHeight);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function Talk({ task, asking = true }: { task: string | null; asking?: boolean }) {
  hook = useStickToBottom(task);
  return (
    <>
      <div ref={hook.ref} data-region="talk">
        <div>
          {asking && <button type="button">Allow</button>}
          <input aria-label="Reason" />
          <textarea aria-label="Answer" />
          <input type="radio" name="pick" aria-label="Keep" />
          <input type="radio" name="pick" aria-label="Replace" />
        </div>
        {hook.away && task && (
          <button type="button" onClick={hook.jump}>
            Latest
          </button>
        )}
      </div>
      <textarea aria-label="Message" />
    </>
  );
}

const region = () => document.querySelector<HTMLElement>("[data-region=talk]")!;
const end = () => height - view;

/** The browser moves the view, and says so. */
function browserScrolls(to: number) {
  act(() => {
    top = Math.max(0, Math.min(to, end()));
    region().dispatchEvent(new Event("scroll"));
  });
}
/**
 * The keyboard closes, as traced: the browser first gives the region all the
 * height the window gained, which can leave the view at the end, and then the
 * app's tab bar comes back into part of it before the scroll is reported.
 */
function keyboardCloses(then?: number) {
  act(() => {
    win += 312;
    const was = top;
    top = Math.max(0, Math.min(top, height - (view + 312)));
    view += 223;
    if (then !== undefined) top = then;
    if (top !== was) region().dispatchEvent(new Event("scroll"));
  });
}
/** The keyboard opens and the region shrinks under the view. */
function keyboardOpens() {
  act(() => {
    win -= 312;
    view -= 223;
  });
  settles();
}
/** Layout has settled and the resize observer reports. */
function settles() {
  act(() => {
    for (const r of resized) r();
  });
}
/** The person drags the view with a finger. */
function drags(to: number) {
  act(() => {
    region().dispatchEvent(new Event("touchmove"));
  });
  browserScrolls(to);
}

/** A key pressed where focus is. */
function presses(key: string) {
  act(() => {
    (document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  });
}
/** Focus moves to an element, as a key or a tap moved it. */
function focuses(el: HTMLElement) {
  act(() => el.focus());
}
/** A finger taps an element: it comes down, focus moves to what it lands on,
    and the tap is over. */
function taps(el: HTMLElement) {
  act(() => {
    el.dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  focuses(el);
  act(() => el.click());
}
const allow = () => document.querySelector("button")!;
const reason = () => document.querySelector("input")!;
const option = (i: number) => document.querySelectorAll<HTMLInputElement>("input[type=radio]")[i]!;
const latest = () => [...document.querySelectorAll("button")].find((b) => b.textContent === "Latest");
const answer = () => region().querySelector("textarea")!;
const composer = () => document.querySelector<HTMLTextAreaElement>("textarea[aria-label=Message]")!;

it("stays on the newest line when the keyboard closes as a new run's lines arrive", () => {
  render(<Talk task="t1" />);
  expect(top).toBe(end());

  // The keyboard closes: the region grows and the browser pulls the view up.
  keyboardCloses();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
  settles();
  expect(top).toBe(end());

  // A line arrives, and the browser scrolls before the view has followed it.
  height = 4388;
  browserScrolls(top - 36);
  expect(hook.away).toBe(false);
  expect(top).toBe(end());

  // Later lines keep it there.
  height = 5100;
  settles();
  expect(top).toBe(end());
  expect(hook.away).toBe(false);
});

it("lets the person read back while lines arrive, and keeps their place", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();

  // A line lands in the same frame as the person's drag.
  height += 90;
  drags(2000);
  expect(hook.away).toBe(true);
  expect(top).toBe(2000);

  height += 400;
  settles();
  expect(top).toBe(2000);
  height += 60;
  browserScrolls(2000);
  expect(top).toBe(2000);
  expect(hook.away).toBe(true);
});

it("counts the wheel, scrolling keys and the scrollbar as the person, but not a tap or typing", () => {
  render(<Talk task="t1" />);
  const leaveAfter = (input: () => void) => {
    act(() => hook.sent());
    act(input);
    height += 120;
    browserScrolls(top - 40);
    return hook.away;
  };

  expect(leaveAfter(() => region().dispatchEvent(new WheelEvent("wheel")))).toBe(true);
  expect(leaveAfter(() => region().dispatchEvent(new KeyboardEvent("keydown", { key: "PageUp", bubbles: true })))).toBe(true);
  expect(leaveAfter(() => region().dispatchEvent(new Event("pointerdown")))).toBe(true);

  // Neither of these moves the view, so the scroll after them is the layout's.
  expect(leaveAfter(() => allow().dispatchEvent(new Event("pointerdown", { bubbles: true })))).toBe(false);
  expect(top).toBe(end());
  expect(leaveAfter(() => reason().dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true })))).toBe(false);
  expect(top).toBe(end());
});

it("lets the keyboard reveal an earlier line as lines arrive, and keeps it in view", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());

  // Tab moves focus from the message field to a line, and the browser reveals
  // it in the frame a new line arrives. The move is small enough to be layout.
  presses("Tab");
  focuses(allow());
  expect(hook.away).toBe(true);
  height += 90;
  const revealed = top - 60;
  browserScrolls(revealed);
  expect(hook.away).toBe(true);
  expect(top).toBe(revealed);

  // Later lines leave the focused line where it is.
  height += 300;
  settles();
  expect(top).toBe(revealed);

  // Tabbing on, back to the message field, leaves the view where it is too:
  // the person is still reading back.
  presses("Tab");
  focuses(composer());
  height += 90;
  settles();
  expect(top).toBe(revealed);
  expect(hook.away).toBe(true);
});

it("follows again when keyboard focus leaves among the newest lines", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());

  // The line is nearly in view: the browser nudges it in, and new lines leave
  // it there.
  presses("Tab");
  focuses(allow());
  expect(hook.away).toBe(true);
  browserScrolls(top - 20);
  expect(hook.away).toBe(true);
  const at = top;
  height += 40;
  settles();
  expect(top).toBe(at);

  presses("Tab");
  focuses(composer());
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
  height += 300;
  settles();
  expect(top).toBe(end());
});

it("lets a screen reader reveal an earlier line as lines arrive", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();

  // A screen reader moves the view without any input the page sees; it goes
  // further than the new line could have moved it.
  height += 90;
  browserScrolls(1200);
  expect(hook.away).toBe(true);
  expect(top).toBe(1200);
  height += 300;
  settles();
  expect(top).toBe(1200);
});

it("follows a small move that stays among the newest lines, whether or not a line arrives with it", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();

  browserScrolls(top - 60);
  expect(hook.away).toBe(false);
  height += 90;
  settles();
  expect(top).toBe(end());

  height += 90;
  browserScrolls(top - 60);
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
});

it("still follows the layout when a tap focuses a line", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(allow());
  height += 90;
  browserScrolls(top - 60);
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
});

it("still follows the layout when a tap follows typing or a Tab", () => {
  render(<Talk task="t1" />);
  settles();

  // Typing a reason, then tapping Deny as the keyboard closes.
  focuses(reason());
  presses("d");
  act(() => {
    allow().dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  focuses(allow());
  keyboardCloses();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());

  // A key that moves no focus, then the app moving focus to a line.
  keyboardOpens();
  focuses(reason());
  presses("Enter");
  focuses(allow());
  expect(hook.away).toBe(false);
  keyboardCloses();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());

  // A Tab, then a tap before focus moves: the tap moved it.
  keyboardOpens();
  focuses(composer());
  presses("Tab");
  act(() => {
    allow().dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  focuses(allow());
  expect(hook.away).toBe(false);
  keyboardCloses();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
});

it("still follows the layout when a tap takes over from the keyboard straight away", () => {
  render(<Talk task="t1" />);
  settles();
  focuses(composer());
  presses("Tab");
  focuses(reason());
  expect(hook.away).toBe(true);

  // Allow, tapped at once, and the keyboard closes.
  taps(allow());
  expect(hook.away).toBe(false);
  keyboardCloses();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
  height += 90;
  settles();
  expect(top).toBe(end());
});

it("does not mistake Latest's smooth scroll for leaving, and lands on lines that arrive meanwhile", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  drags(1000);
  expect(hook.away).toBe(true);

  act(() => hook.jump());
  expect(scrolledTo.at(-1)).toMatchObject({ behavior: "smooth" });
  // On its way down the view passes positions far from the end.
  browserScrolls(1600);
  browserScrolls(2400);
  expect(hook.away).toBe(true);

  // A line arrives mid-scroll: the view goes straight to it.
  height += 300;
  settles();
  expect(top).toBe(end());
  browserScrolls(top);
  expect(hook.away).toBe(false);
  height += 200;
  settles();
  expect(top).toBe(end());
});

it("stops the jump where the person takes over, or where the browser gave up", () => {
  render(<Talk task="t1" />);
  drags(1000);

  act(() => hook.jump());
  browserScrolls(1800);
  drags(1500);
  height += 300;
  settles();
  expect(top).toBe(1500);
  expect(hook.away).toBe(true);

  act(() => hook.jump());
  browserScrolls(2600);
  act(() => {
    region().dispatchEvent(new Event("scrollend"));
  });
  expect(hook.away).toBe(true);
  height += 300;
  settles();
  expect(top).toBe(2600);
});

it("goes back to the newest line on send, and stays through the keyboard closing", () => {
  render(<Talk task="t1" />);
  drags(500);
  expect(hook.away).toBe(true);

  act(() => hook.sent());
  expect(hook.away).toBe(false);
  expect(top).toBe(end());

  keyboardCloses();
  height += 84;
  browserScrolls(top - 36);
  settles();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
});

it("lets a screen reader reveal an earlier line as the keyboard closes", () => {
  render(<Talk task="t1" />);

  // The reveal goes further than the tab bar coming back leaves the view.
  keyboardCloses(3676);
  expect(hook.away).toBe(true);
  expect(top).toBe(3676);
  settles();
  expect(top).toBe(3676);

  // Nor does a region that grows as much as the window leave the view short.
  keyboardOpens();
  act(() => hook.sent());
  act(() => {
    win += 223;
    view += 223;
    top = end() - 150;
    region().dispatchEvent(new Event("scroll"));
  });
  expect(hook.away).toBe(true);
  expect(top).toBe(end() - 150);
});

it("follows again when the keyboard chooses Latest", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  drags(1000);
  focuses(composer());

  presses("Tab");
  focuses(latest()!);
  presses("Enter");
  act(() => latest()!.click());
  expect(scrolledTo.at(-1)).toMatchObject({ behavior: "smooth" });
  browserScrolls(2400);
  browserScrolls(end());
  expect(hook.away).toBe(false);
  height += 300;
  settles();
  expect(top).toBe(end());
});

it("keeps holding the view while arrows move on through a line's options", () => {
  let now = 10_000;
  vi.spyOn(performance, "now").mockImplementation(() => now);
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));
  expect(hook.away).toBe(true);

  // Later, an arrow moves to the other option, and the browser nudges it in.
  now += 1500;
  presses("ArrowUp");
  focuses(option(0));
  browserScrolls(top - 40);
  expect(hook.away).toBe(true);
  const at = top;
  height += 300;
  settles();
  expect(top).toBe(at);

  // A tap takes over, even on the same line.
  now += 1500;
  height -= 300;
  browserScrolls(at);
  taps(allow());
  expect(hook.away).toBe(false);
  height += 90;
  settles();
  expect(top).toBe(end());
});

it("follows again once a tap is over, on the focused control or another, and moves nothing before", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));
  height += 40;
  settles();
  expect(hook.away).toBe(true);
  const at = top;

  // The finger comes down on the option that has focus, so focus stays, and a
  // line arrives before the tap is over.
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  height += 40;
  settles();
  expect(top).toBe(at);
  expect(hook.away).toBe(true);
  act(() => option(1).click());
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
  height += 300;
  settles();
  expect(top).toBe(end());

  // Nor does a tap that moves focus to another control.
  focuses(composer());
  presses("Tab");
  focuses(option(0));
  height += 40;
  settles();
  const then = top;
  act(() => {
    allow().dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  focuses(allow());
  expect(top).toBe(then);
  expect(hook.away).toBe(true);
  act(() => allow().click());
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
});

it("moves nothing before a tap is over, even when the browser brings the view to the end", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));
  height += 40;
  settles();
  expect(hook.away).toBe(true);

  // The finger holds the option while the region grows, and the browser
  // settles the view at the end before the tap is over.
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  view += 60;
  browserScrolls(top);
  expect(top).toBe(end());
  expect(hook.away).toBe(true);
  height += 40;
  settles();
  expect(top).toBe(end() - 40);
  expect(hook.away).toBe(true);
  act(() => option(1).click());
  expect(hook.away).toBe(false);
  expect(top).toBe(end());

  // Once it is over, the layout's scrolls among the newest lines are followed.
  browserScrolls(end() - 10);
  expect(hook.away).toBe(false);
});

it("lets the next press or key end a press that never became a tap", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));

  // A long press, which the browser does not count as a tap, keeps the view
  // until the next press, wherever it lands.
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  browserScrolls(end());
  expect(hook.away).toBe(true);
  act(() => {
    composer().dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  browserScrolls(end());
  expect(hook.away).toBe(false);

  // Or until the next key.
  focuses(composer());
  presses("Tab");
  focuses(option(1));
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  browserScrolls(end());
  expect(hook.away).toBe(true);
  presses("Escape");
  browserScrolls(end());
  expect(hook.away).toBe(false);
});

it("follows again when a message sent earlier is taken during a press that never became a tap", () => {
  const { rerender } = render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));

  // A finger holds the option near the end when the message is taken, which
  // takes the view to the end, and lets go without a tap.
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  height += 120;
  act(() => hook.sent());
  browserScrolls(top);
  expect(hook.away).toBe(false);
  expect(top).toBe(end());

  // The reply follows.
  height += 300;
  settles();
  expect(top).toBe(end());
  expect(hook.away).toBe(false);

  // Nor does a press held when another conversation opens, as from a
  // notification, hold that one.
  focuses(composer());
  presses("Tab");
  focuses(option(1));
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  rerender(<Talk task="t2" />);
  browserScrolls(top);
  expect(hook.away).toBe(false);
  height += 300;
  settles();
  expect(top).toBe(end());
});

it("holds the view again when arrows move on from where a finger put focus", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));

  // A finger taps the option, and the view follows again.
  taps(option(1));
  expect(hook.away).toBe(false);
  height += 90;
  settles();
  expect(top).toBe(end());

  // An arrow moves to the other option, and the browser nudges it in.
  presses("ArrowUp");
  focuses(option(0));
  browserScrolls(top - 40);
  expect(hook.away).toBe(true);
  const at = top;
  height += 300;
  settles();
  expect(top).toBe(at);

  // An arrow that moves the caret outside, and then the app moving focus in,
  // hold nothing.
  focuses(composer());
  drags(end());
  expect(hook.away).toBe(false);
  presses("ArrowLeft");
  focuses(allow());
  expect(hook.away).toBe(false);

  // Nor does an arrow before a key that moves nothing, such as Enter on a
  // control, whose action moves focus.
  presses("ArrowDown");
  presses("Enter");
  focuses(reason());
  expect(hook.away).toBe(false);

  // Nor an arrow before a tap, once another key has ended the tap.
  presses("ArrowDown");
  taps(reason());
  presses("Enter");
  focuses(allow());
  expect(hook.away).toBe(false);
  height += 90;
  settles();
  expect(top).toBe(end());
});

it("keeps following when keys move a caret in a field there and the app then moves focus", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();

  // A finger puts focus in a field in the conversation, and a key moves its
  // caret. The answer is acknowledged a moment later and the app moves focus
  // on, and the view keeps following.
  for (const field of [answer(), reason()]) {
    for (const key of ["Home", "End", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"]) {
      taps(field);
      presses(key);
      focuses(allow());
      expect(hook.away).toBe(false);
      height += 90;
      settles();
      expect(top).toBe(end());
    }
  }
});

it("lets a finger that scrolls take over from the keyboard", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(option(1));

  // The finger comes down on the focused option and drags the view back.
  act(() => {
    option(1).dispatchEvent(new Event("pointerdown", { bubbles: true }));
  });
  drags(1000);
  expect(hook.away).toBe(true);
  height += 300;
  settles();
  expect(top).toBe(1000);

  // Dragged back to the end, the view follows again, and a keyboard's click
  // later takes nothing over.
  drags(end());
  expect(hook.away).toBe(false);
  height += 90;
  settles();
  expect(top).toBe(end());
  drags(1000);
  act(() => option(1).click());
  expect(hook.away).toBe(true);
  expect(top).toBe(1000);
});

it("keeps the person's place when a control reached by keyboard opens another screen", () => {
  render(<Talk task="t1" />);
  view = 478;
  settles();
  drags(1000);
  const shown = { height, view };
  /** Another screen hides the conversation, which has no size while hidden. */
  const hides = () =>
    act(() => {
      height = 0;
      view = 0;
      top = 0;
    });
  /** Back again, the shell puts the view where it was, and a line arrives. */
  const returns = () => {
    act(() => {
      ({ height, view } = shown);
      top = 1000;
    });
    settles();
    height += 300;
    shown.height = height;
    settles();
  };

  // Tab to a control among earlier lines and open it: the conversation is
  // hidden, and the layout reports it, before focus moves to the new screen.
  focuses(composer());
  presses("Tab");
  focuses(allow());
  hides();
  settles();
  focuses(composer());
  returns();
  expect(top).toBe(1000);
  expect(hook.away).toBe(true);

  // Or focus moves first.
  presses("Tab");
  focuses(allow());
  hides();
  focuses(composer());
  settles();
  returns();
  expect(top).toBe(1000);
  expect(hook.away).toBe(true);

  // Or a scroll already on its way arrives once the conversation is hidden.
  drags(1000);
  hides();
  act(() => {
    region().dispatchEvent(new Event("scroll"));
  });
  returns();
  expect(top).toBe(1000);
  expect(hook.away).toBe(true);

  // Or a finger taps it, and the conversation is hidden before the tap is over.
  presses("Tab");
  focuses(allow());
  allow().addEventListener("click", hides, { once: true });
  taps(allow());
  focuses(composer());
  settles();
  returns();
  expect(top).toBe(1000);
  expect(hook.away).toBe(true);
});

it("follows again when the focused line goes away among the newest lines", () => {
  const { rerender } = render(<Talk task="t1" />);
  view = 478;
  settles();
  focuses(composer());
  presses("Tab");
  focuses(allow());
  expect(hook.away).toBe(true);

  // Answered, the line's controls go, and the next line arrives with them.
  rerender(<Talk task="t1" asking={false} />);
  height += 90;
  settles();
  expect(hook.away).toBe(false);
  expect(top).toBe(end());
});

it("reads a new task's page from the top and does not follow it", () => {
  render(<Talk task={null} />);
  expect(top).toBe(0);
  height += 500;
  settles();
  expect(top).toBe(0);
});
