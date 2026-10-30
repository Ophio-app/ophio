// SPDX-License-Identifier: Apache-2.0
// Runs in Ophio's own isolated world of one frame, where page scripts can't
// reach it, as fn(op, a, b, c, d). Returns plain values only. Element ids are
// good until the next look at this frame, and only in this document.
function (op, a, b, c, d) {
  const S = globalThis.__ophioAgent || (globalThis.__ophioAgent = {
    nodes: new Map(), next: 1, filled: new WeakSet(), filledList: [],
  });
  const ROLES = new Set(["button", "link", "checkbox", "radio", "tab", "menuitem",
    "menuitemcheckbox", "menuitemradio", "option", "switch", "combobox", "textbox",
    "searchbox", "slider", "spinbutton", "listbox", "treeitem"]);
  const TEXT = ["text", "search", "url", "email", "tel", "number"];
  const SECRET = /pass|pwd|secret|token|otp|one-?time|cvv|cvc|csc|card|ssn|social|security|answer|(^|[^a-z])pin([^a-z]|$)/i;
  // Text as the page has it, up to RAW_TEXT characters (as in ports) and then
  // "…": Ophio masks secrets in it before closing up its spaces and cutting it.
  const RAW_TEXT = 2048;
  const whole = s => {
    s = s || "";
    if (s.length <= RAW_TEXT) return s;
    const cut = s.slice(0, RAW_TEXT);
    return (/[\ud800-\udbff]$/.test(cut) ? cut.slice(0, -1) : cut) + "…";
  };
  const clean = s => (s || "").trim() ? whole(s) : "";
  const node = id => { const e = S.nodes.get(id); return e && e.isConnected ? e : null; };
  const deepActive = () => {
    let e = document.activeElement;
    while (e && e.shadowRoot && e.shadowRoot.activeElement) e = e.shadowRoot.activeElement;
    return e;
  };
  const deepPoint = (x, y) => {
    let e = document.elementFromPoint(x, y);
    while (e && e.shadowRoot) {
      const inner = e.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === e) break;
      e = inner;
    }
    return e;
  };
  const all = (root, out) => {
    for (const e of root.querySelectorAll("*")) {
      out.push(e);
      if (e.shadowRoot) all(e.shadowRoot, out);
    }
    return out;
  };
  const shown = e => e.isConnected
    && (!e.checkVisibility || e.checkVisibility({ opacityProperty: true, visibilityProperty: true }));
  const inView = r => r.width > 0 && r.height > 0 && r.right > 0 && r.bottom > 0
    && r.left < innerWidth && r.top < innerHeight;
  const field = e => e.localName === "textarea" || e.localName === "select" || e.isContentEditable
    || (e.localName === "input" && !["button", "submit", "reset", "image", "checkbox", "radio",
      "range", "color", "file", "hidden"].includes(e.type));
  const interactive = e => {
    const tag = e.localName;
    if (tag === "input") return e.type !== "hidden";
    if (tag === "a") return e.hasAttribute("href");
    if (["button", "select", "textarea", "summary"].includes(tag)) return true;
    if (ROLES.has(e.getAttribute("role"))) return true;
    if (e.isContentEditable && !(e.parentElement && e.parentElement.isContentEditable)) return true;
    const tab = e.getAttribute("tabindex");
    return (tab !== null && +tab >= 0) || e.hasAttribute("onclick");
  };
  const roleOf = e => {
    const role = e.getAttribute("role");
    if (ROLES.has(role)) return role;
    const tag = e.localName;
    if (tag === "a") return "link";
    if (tag === "select") return e.multiple ? "listbox" : "combobox";
    if (tag === "textarea" || e.isContentEditable) return "textbox";
    if (tag === "input") {
      if (["checkbox", "radio"].includes(e.type)) return e.type;
      if (["button", "submit", "reset", "image", "file", "color"].includes(e.type)) return "button";
      if (e.type === "range") return "slider";
      if (e.type === "number") return "spinbutton";
      if (e.type === "search") return "searchbox";
      return "textbox";
    }
    return "button";
  };
  const nameOf = e => {
    const label = clean(e.getAttribute("aria-label"));
    if (label) return label;
    const by = e.getAttribute("aria-labelledby");
    if (by) {
      const root = e.getRootNode();
      const text = clean(by.split(/\s+/).map(id => root.getElementById && root.getElementById(id))
        .filter(Boolean).map(n => n.textContent).join(" "));
      if (text) return text;
    }
    if (field(e)) {
      // A field's own text is its value, so it never names it.
      const labels = clean([...(e.labels || [])].map(l => l.textContent).join(" "));
      return labels || clean(e.getAttribute("placeholder") || e.getAttribute("title")
        || e.getAttribute("name"));
    }
    if (e.localName === "input") return clean(e.value || e.getAttribute("alt") || e.getAttribute("title"));
    const text = clean(e.innerText);
    if (text) return text;
    const image = e.querySelector("img[alt]");
    return clean(e.getAttribute("title") || (image && image.getAttribute("alt")));
  };
  const secret = e => S.filled.has(e) || (e.localName === "input" && e.type === "password")
    || SECRET.test([e.getAttribute("autocomplete"), e.getAttribute("name"), e.id,
      e.getAttribute("aria-label"), e.getAttribute("placeholder")].join(" "));
  const valueOf = e => {
    if (secret(e)) return null;
    if (e.localName === "select") return clean([...e.selectedOptions].map(o => o.text).join(", "));
    if (e.localName === "textarea" || (e.localName === "input" && TEXT.includes(e.type))) {
      return whole(e.value);
    }
    if (e.isContentEditable) return whole(e.innerText);
    return null;
  };
  // Brings an element into its frame's view, if it isn't.
  const bring = e => {
    const r = e.getBoundingClientRect();
    const x = r.x + r.width / 2, y = r.y + r.height / 2;
    if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) {
      e.scrollIntoView({ block: "center", inline: "center" });
    }
  };
  const box = e => { const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; };
  // A field a person could see and type in: shown, big enough, in view and
  // not covered by anything but its own label.
  const usable = e => {
    if (!e || !shown(e) || e.disabled || e.readOnly) return false;
    const r = e.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) return false;
    const x = r.x + r.width / 2, y = r.y + r.height / 2;
    if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) return false;
    const hit = deepPoint(x, y);
    return !!hit && (hit === e || e.contains(hit)
      || [...(e.labels || [])].some(l => l === hit || l.contains(hit)));
  };
  const onSite = (host, sites) => sites.some(site => {
    const s = site.toLowerCase().replace(/\.$/, "").replace(/^www\./, "");
    return s && (host === s || host.endsWith("." + s));
  });
  // Why this frame may not receive a login's details, if it may not.
  const unsafe = sites => {
    if (location.protocol !== "https:") return "This page is not secure";
    if (!onSite(location.hostname.toLowerCase().replace(/\.$/, ""), sites)) {
      return "This page is not on the login's site";
    }
    for (const origin of location.ancestorOrigins || []) {
      let url;
      try { url = new URL(origin); } catch { return "This form is inside another site's page"; }
      if (url.protocol !== "https:" || !onSite(url.hostname.toLowerCase().replace(/\.$/, ""), sites)) {
        return "This form is inside another site's page";
      }
    }
    return null;
  };
  const register = e => { const id = S.next++; S.nodes.set(id, e); return id; };
  const inputs = () => all(document, []).filter(e => e.localName === "input");
  const textual = e => e.localName === "input" && ["text", "email", "tel"].includes(e.type);
  const USERNAME = /user|email|login|account|identifier/i;
  // Puts `value` in a field as typing would, or leaves it empty.
  const put = (e, value, password) => {
    const fits = () => usable(e) && (password ? e.type === "password" : textual(e));
    bring(e);
    if (!fits()) return false;
    if (password) e.style.setProperty("-webkit-text-security", "disc", "important");
    e.focus();
    e.select();
    // The page's focus handlers have run: the field must still be the one checked.
    if (deepActive() !== e || !fits()) return false;
    const typed = document.execCommand("insertText", false, value);
    // A script that answered the typing may have changed the field.
    if (!typed || e.value !== value || !e.isConnected || (password && e.type !== "password")) {
      e.value = "";
      return false;
    }
    S.filled.add(e);
    S.filledList.push(new WeakRef(e));
    return true;
  };
  // Found fields, whether this frame has the keyboard, and where the first is.
  const found = (ids, e) => ({ ...ids, focused: document.hasFocus(), at: box(e) });

  switch (op) {
    case "observe": {
      S.nodes = new Map();
      const out = [];
      const active = deepActive();
      for (const e of all(document, [])) {
        if (out.length >= 200) break;
        if (!interactive(e) || !shown(e)) continue;
        const r = e.getBoundingClientRect();
        if (!inView(r)) continue;
        const role = roleOf(e);
        out.push({
          id: register(e), role, name: nameOf(e), value: valueOf(e),
          x: r.x, y: r.y, w: r.width, h: r.height,
          disabled: !!(e.disabled || e.getAttribute("aria-disabled") === "true"),
          focused: e === active,
          checked: ["checkbox", "radio", "switch", "menuitemcheckbox", "menuitemradio"].includes(role)
            ? (e.localName === "input" ? e.checked : e.getAttribute("aria-checked") === "true") : null,
        });
      }
      return out;
    }
    case "rect": {
      const e = node(a);
      if (!e) return null;
      bring(e);
      return box(e);
    }
    // Whether the control has the keyboard; with `b`, selects what it holds
    // so typing replaces it.
    case "prepare": {
      const e = node(a);
      const focused = deepActive();
      if (!e || !(focused === e || e.contains(focused))) return false;
      if (b) {
        if (typeof e.select === "function") {
          try { e.select(); } catch { /* fields without a selection */ }
        } else if (e.isContentEditable) {
          const range = document.createRange();
          range.selectNodeContents(e);
          getSelection().removeAllRanges();
          getSelection().addRange(range);
        }
      }
      return true;
    }
    case "ready":
      return document.readyState;
    case "findLogin": {
      // A frame with nothing to fill says so, whatever site it's from.
      const problem = unsafe(a);
      if (problem) {
        return { error: inputs().some(e => e.type === "password" || textual(e)) ? problem : "There is no sign-in field here" };
      }
      const fields = inputs();
      const password = fields.filter(e => e.type === "password").find(e => (bring(e), usable(e)));
      const candidates = fields.filter(e => textual(e) && (!password
        || password.compareDocumentPosition(e) & Node.DOCUMENT_POSITION_PRECEDING));
      const named = candidates.filter(e => /username|email/.test(e.getAttribute("autocomplete") || "")
        || e.type === "email" || USERNAME.test((e.getAttribute("name") || "") + " " + e.id));
      // The field nearest before the password, or one that says what it is.
      const ordered = password ? [...named.reverse(), ...candidates.reverse()] : named;
      const username = ordered.find(e => (bring(e), usable(e)));
      if (!password && !username) return { error: "There is no sign-in field here" };
      return found({ username: username ? register(username) : null, password: password ? register(password) : null },
        username || password);
    }
    case "fillLogin": {
      const problem = unsafe(a);
      if (problem) return { error: problem };
      const username = b == null ? null : node(b);
      const password = c == null ? null : node(c);
      if ((b != null && !username) || (c != null && !password)) return { error: "The page changed; try again" };
      if (username && !(textual(username) && usable(username))) return { error: "The username field can't be used" };
      if (password && !(password.type === "password" && (bring(password), usable(password)))) {
        return { error: "The password field can't be used" };
      }
      if (username && d.username !== null && !put(username, d.username, false)) {
        return { error: "The username did not go in" };
      }
      if (password && !put(password, d.password, true)) return { error: "The password did not go in" };
      return { ok: true };
    }
    case "findCode": {
      const problem = unsafe(a);
      if (problem) {
        return { error: inputs().some(e => ["text", "tel", "number", "password"].includes(e.type)) ? problem : "There is no code field here" };
      }
      const length = b;
      const fields = inputs().filter(e => ["text", "tel", "number", "password"].includes(e.type)
        && (bring(e), usable(e)));
      const marked = fields.find(e => (e.getAttribute("autocomplete") || "").includes("one-time-code"));
      if (marked) return found({ fields: [register(marked)] }, marked);
      const boxes = fields.filter(e => e.maxLength === 1);
      if (boxes.length >= length) return found({ fields: boxes.slice(0, length).map(register) }, boxes[0]);
      const named = fields.find(e => /otp|code|totp|2fa|mfa|verif|token|pin/i.test(
        [e.getAttribute("name"), e.id, e.getAttribute("aria-label"), e.getAttribute("placeholder")].join(" ")));
      if (named) return found({ fields: [register(named)] }, named);
      return { error: "There is no code field here" };
    }
    case "fillCode": {
      const problem = unsafe(a);
      if (problem) return { error: problem };
      const fields = b.map(node);
      if (fields.some(e => !e)) return { error: "The page changed; try again" };
      const parts = fields.length === 1 ? [c] : [...c];
      if (parts.length !== fields.length) return { error: "The code does not fit these fields" };
      for (let i = 0; i < fields.length; i++) {
        const e = fields[i];
        const fits = () => usable(e) && ["text", "tel", "number", "password"].includes(e.type);
        bring(e);
        if (!fits()) return { error: "The code field can't be used" };
        e.focus();
        e.select();
        if (deepActive() !== e || !fits()) return { error: "The code field can't be used" };
        if (!document.execCommand("insertText", false, parts[i]) || e.value !== parts[i]) {
          e.value = "";
          return { error: "The code did not go in" };
        }
        S.filled.add(e);
        S.filledList.push(new WeakRef(e));
      }
      return { ok: true };
    }
    // Where filled fields are in this frame's view, to be painted over.
    case "filledRects": {
      S.filledList = S.filledList.filter(w => { const e = w.deref(); return e && e.isConnected; });
      return S.filledList.map(w => box(w.deref())).filter(r => r.w > 0 && r.h > 0);
    }
  }
  return null;
}
