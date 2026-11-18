//! The scripts the agent browser tools run in the page
//! (`specs/SESSION_BROWSER_PRD.md` §6, "How the Desktop does them").
//!
//! - [`INIT`] runs at the start of every page: it keeps the page's console
//!   messages, uncaught errors and failed promises, and tells the window
//!   when the user clicks or types in the page (a trusted event), which
//!   takes the browser over from a driving agent.
//! - [`snapshot`] reads the page as an outline and remembers every element
//!   an agent can act on under a ref (`e1`, `e2`, …), which [`click`],
//!   [`type_text`] and [`press`] act on with events fired from script (v1;
//!   real input later where pages reject fired events).
//! - [`eval_start`] / [`eval_poll`] run an agent's expression in two steps,
//!   so a promise can be awaited.
//!
//! Every script returns a string of JSON (wry's callback hands back the
//! value JSON-encoded, so [`decode`] unwraps it twice), and every value from
//! the agent is put in with `serde_json`, never pasted into the source.

/// What a page posts through `window.ipc` when the user uses it.
pub const USER_INPUT_MESSAGE: &str = "flightdeck:user-input";

/// Runs first in every page (see the module docs).
pub const INIT: &str = r#"(() => {
  if (window.__fd) return;
  const fd = (window.__fd = { console: [], refs: new Map() });
  const MAX = 300;
  const text = (a) => {
    if (typeof a === "string") return a;
    if (a instanceof Error) return (a.name || "Error") + ": " + a.message + (a.stack ? "\n" + a.stack : "");
    try { const s = JSON.stringify(a); return s === undefined ? String(a) : s; } catch (_) { return String(a); }
  };
  const push = (level, args) => {
    try {
      fd.console.push({ level, text: Array.from(args).map(text).join(" ").slice(0, 2000), at: Date.now() });
      if (fd.console.length > MAX) fd.console.splice(0, fd.console.length - MAX);
    } catch (_) {}
  };
  for (const level of ["log", "info", "warn", "error", "debug"]) {
    const original = console[level];
    console[level] = function (...args) { push(level, args); return original.apply(this, args); };
  }
  addEventListener("error", (e) => {
    if (e.message) push("error", [e.message + (e.filename ? " (" + e.filename + ":" + e.lineno + ")" : "")]);
  }, true);
  addEventListener("unhandledrejection", (e) => push("error", ["Unhandled promise rejection:", e.reason]));
  let last = 0;
  const used = (e) => {
    if (!e.isTrusted) return;
    const now = Date.now();
    if (now - last < 400) return;
    last = now;
    try { window.ipc.postMessage("flightdeck:user-input"); } catch (_) {}
  };
  addEventListener("pointerdown", used, true);
  addEventListener("keydown", used, true);
})();"#;

/// Shared by the scripts below: the refs, and how an element is described.
const PRELUDE: &str = r#"
  const fd = window.__fd || (window.__fd = { console: [], refs: new Map() });
  const clean = (s, n) => String(s || "").replace(/\s+/g, " ").trim().slice(0, n || 120);
  const nameOf = (el) => clean(
    el.getAttribute("aria-label") ||
    (el.getAttribute("aria-labelledby") && Array.from(el.getAttribute("aria-labelledby").split(/\s+/)).map((id) => { const l = document.getElementById(id); return l ? l.innerText : ""; }).join(" ")) ||
    (el.labels && el.labels.length ? el.labels[0].innerText : "") ||
    el.getAttribute("alt") || el.getAttribute("title") ||
    (["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) ? el.getAttribute("placeholder") || el.getAttribute("name") || "" : el.innerText || el.textContent) ||
    (el.tagName === "INPUT" && ["submit", "button", "reset"].includes(el.type) ? el.value : "") || ""
  );
  const roleOf = (el) => {
    const r = el.getAttribute("role");
    if (r) return r;
    const t = el.tagName;
    if (t === "A") return "link";
    if (t === "BUTTON" || t === "SUMMARY") return "button";
    if (t === "SELECT") return "combobox";
    if (t === "TEXTAREA") return "textbox";
    if (t === "INPUT") {
      const type = (el.type || "text").toLowerCase();
      if (["submit", "button", "reset", "image"].includes(type)) return "button";
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (type === "range") return "slider";
      if (type === "file") return "file input";
      return "textbox";
    }
    if (el.isContentEditable) return "textbox";
    return "clickable";
  };
  const describe = (el) => roleOf(el) + ' "' + nameOf(el) + '"';
  const byRef = (ref) => {
    const el = fd.refs.get(ref);
    if (!el) return { error: "There is no element " + ref + ". Take a new browser_snapshot: refs change when the page does." };
    if (!el.isConnected) return { error: "Element " + ref + " is no longer on the page. Take a new browser_snapshot." };
    return { el };
  };
  const center = (el) => {
    el.scrollIntoView({ block: "center", inline: "center" });
    const r = el.getBoundingClientRect();
    return { clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
  };
  const setValue = (el, value) => {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
      : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, "value").set;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const key = (target, k, type) => target.dispatchEvent(new KeyboardEvent(type, {
    key: k, code: k.length === 1 ? (/[a-z]/i.test(k) ? "Key" + k.toUpperCase() : /[0-9]/.test(k) ? "Digit" + k : "") : k,
    keyCode: k === "Enter" ? 13 : k === "Escape" ? 27 : k === "Tab" ? 9 : k === "Backspace" ? 8 : k.length === 1 ? k.toUpperCase().charCodeAt(0) : 0,
    bubbles: true, cancelable: true, composed: true,
  }));
  const editable = (el) => el && (el.isContentEditable || el instanceof HTMLTextAreaElement ||
    (el instanceof HTMLInputElement && !["checkbox", "radio", "submit", "button", "reset", "image", "file", "range", "color"].includes((el.type || "text").toLowerCase())));
  const submit = (el) => {
    const form = el.form || (el.closest && el.closest("form"));
    if (!form) return false;
    if (form.requestSubmit) form.requestSubmit(); else form.submit();
    return true;
  };
"#;

/// Wrap `body` (which may use the prelude and must `return` a value) as a
/// script answering a JSON string.
fn script(body: &str) -> String {
    format!(
        "(() => {{ try {{{PRELUDE}\n{body}\n}} catch (e) {{ return JSON.stringify({{ error: String((e && e.message) || e) }}); }} }})()"
    )
}

/// `value` as a JavaScript literal.
fn lit(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

/// Read the page as an outline with refs (`browser_snapshot`). Answers
/// `{title, url, text, truncated}`.
pub fn snapshot() -> String {
    script(
        r#"
  fd.refs = new Map();
  let n = 0, size = 0, truncated = false;
  const LIMIT = 40000;
  const lines = [];
  const emit = (depth, line) => {
    if (truncated) return;
    const s = "  ".repeat(depth) + "- " + line;
    size += s.length + 1;
    if (size > LIMIT) { truncated = true; return; }
    lines.push(s);
  };
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "HEAD", "META", "LINK", "IFRAME"]);
  const INTERACTIVE = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[role=textbox],[role=combobox],[role=searchbox],[contenteditable=""],[contenteditable=true],[onclick]';
  const LANDMARK = { NAV: "navigation", MAIN: "main", HEADER: "banner", FOOTER: "contentinfo", ASIDE: "complementary", FORM: "form", DIALOG: "dialog", TABLE: "table", UL: "list", OL: "list" };
  const hidden = (el) => {
    if (el.hidden || el.getAttribute("aria-hidden") === "true") return true;
    const s = getComputedStyle(el);
    if (s.display === "none" || s.visibility === "hidden") return true;
    return el.getClientRects().length === 0 && s.display !== "contents";
  };
  const ref = (el) => { const r = "e" + ++n; fd.refs.set(r, el); return r; };
  const field = (el) => {
    const bits = [];
    if (el instanceof HTMLSelectElement) {
      const opts = Array.from(el.options).slice(0, 30).map((o) => (o.selected ? "*" : "") + clean(o.text, 40));
      bits.push("options: " + opts.join(" | "));
    } else if (el.type === "checkbox" || el.type === "radio") {
      bits.push(el.checked ? "checked" : "unchecked");
    } else if ("value" in el && el.type !== "password" && !["submit", "button", "reset"].includes(el.type)) {
      bits.push('value="' + clean(el.value, 200) + '"');
    } else if (el.isContentEditable) {
      bits.push('value="' + clean(el.innerText, 200) + '"');
    }
    if (el.disabled) bits.push("disabled");
    if (el.getAttribute("aria-expanded")) bits.push("expanded=" + el.getAttribute("aria-expanded"));
    if (el.getAttribute("aria-selected") === "true") bits.push("selected");
    return bits.length ? " " + bits.join(" ") : "";
  };
  // A label's own words are its field's name: said once, on the field.
  const labelled = (node) => node.tagName === "LABEL" && node.querySelector(INTERACTIVE);
  const walk = (node, depth) => {
    const quiet = labelled(node);
    for (const child of node.childNodes) {
      if (truncated) return;
      if (child.nodeType === Node.TEXT_NODE) {
        if (quiet) continue;
        const t = clean(child.textContent, 300);
        if (t) emit(depth, 'text "' + t + '"');
        continue;
      }
      if (child.nodeType !== Node.ELEMENT_NODE || SKIP.has(child.tagName) || hidden(child)) continue;
      const el = child;
      if (el.matches(INTERACTIVE)) {
        let line = describe(el) + " [ref=" + ref(el) + "]" + field(el);
        if (el.tagName === "A") {
          const href = el.getAttribute("href") || "";
          if (href && !href.startsWith("javascript:")) line += " -> " + clean(href, 120);
        }
        emit(depth, line);
        // A link or button's own text is its name; what it holds that can
        // be acted on is still listed.
        if (el.querySelector(INTERACTIVE)) walk(el, depth + 1);
        continue;
      }
      const h = /^H([1-6])$/.exec(el.tagName);
      if (h) { emit(depth, 'heading "' + clean(el.innerText, 200) + '" [level=' + h[1] + "]"); continue; }
      if (el.tagName === "IMG") { const alt = clean(el.getAttribute("alt"), 120); if (alt) emit(depth, 'img "' + alt + '"'); continue; }
      if (el.tagName === "svg" || el.tagName === "SVG") continue;
      const landmark = el.getAttribute("role") || LANDMARK[el.tagName];
      const inner = el.shadowRoot || el;
      if (landmark) {
        const label = el.getAttribute("aria-label");
        emit(depth, landmark + (label ? ' "' + clean(label) + '"' : "") + ":");
        walk(inner, depth + 1);
      } else if (el.tagName === "LI" && !el.querySelector(INTERACTIVE)) {
        const t = clean(el.innerText, 300);
        if (t) emit(depth, 'listitem "' + t + '"');
      } else if (/^(P|LABEL|PRE|BLOCKQUOTE|TD|TH|DT|DD|FIGCAPTION|SPAN)$/.test(el.tagName) && !el.querySelector(INTERACTIVE + ",h1,h2,h3,h4,h5,h6,img")) {
        const t = clean(el.innerText, 300);
        if (t) emit(depth, 'text "' + t + '"');
      } else {
        walk(inner, depth);
      }
    }
  };
  walk(document.body || document.documentElement, 0);
  const dialog = document.querySelector("dialog[open], [role=dialog][aria-modal=true]");
  return JSON.stringify({ title: document.title, url: location.href, text: lines.join("\n"), truncated, dialog: !!dialog });
"#,
    )
}

/// Click element `r#ref` (`browser_click`). Answers `{desc}` or `{error}`.
pub fn click(r#ref: &str) -> String {
    script(&format!(
        r#"
  const found = byRef({r});
  if (found.error) return JSON.stringify(found);
  const el = found.el;
  const at = center(el);
  const base = {{ bubbles: true, cancelable: true, composed: true, view: window, button: 0, ...at }};
  el.dispatchEvent(new PointerEvent("pointerover", {{ ...base, pointerType: "mouse", isPrimary: true }}));
  el.dispatchEvent(new MouseEvent("mouseover", base));
  el.dispatchEvent(new PointerEvent("pointerdown", {{ ...base, pointerType: "mouse", isPrimary: true, buttons: 1 }}));
  el.dispatchEvent(new MouseEvent("mousedown", {{ ...base, buttons: 1 }}));
  if (el.focus) el.focus({{ preventScroll: true }});
  el.dispatchEvent(new PointerEvent("pointerup", {{ ...base, pointerType: "mouse", isPrimary: true }}));
  el.dispatchEvent(new MouseEvent("mouseup", base));
  el.click();
  return JSON.stringify({{ desc: describe(el) }});
"#,
        r = lit(r#ref)
    ))
}

/// Put `text` in field `r#ref`, then press Enter when `submit`
/// (`browser_type`). Answers `{desc}` or `{error}`.
pub fn type_text(r#ref: &str, text: &str, submit: bool) -> String {
    script(&format!(
        r#"
  const found = byRef({r});
  if (found.error) return JSON.stringify(found);
  const el = found.el;
  const text = {t};
  center(el);
  if (el.focus) el.focus({{ preventScroll: true }});
  if (el instanceof HTMLSelectElement) {{
    const want = text.trim().toLowerCase();
    const opt = Array.from(el.options).find((o) => o.value.toLowerCase() === want || o.text.trim().toLowerCase() === want);
    if (!opt) return JSON.stringify({{ error: "That list has no option " + JSON.stringify(text) + "." }});
    setValue(el, opt.value);
  }} else if (el.isContentEditable) {{
    const range = document.createRange();
    range.selectNodeContents(el);
    const sel = getSelection();
    sel.removeAllRanges();
    sel.addRange(range);
    if (!document.execCommand("insertText", false, text)) el.textContent = text;
    el.dispatchEvent(new Event("input", {{ bubbles: true, composed: true }}));
  }} else if (editable(el)) {{
    setValue(el, text);
  }} else {{
    return JSON.stringify({{ error: describe(el) + " is not a field you can type into." }});
  }}
  if ({s}) {{
    const go = key(el, "Enter", "keydown");
    key(el, "Enter", "keypress");
    if (go && !(el instanceof HTMLTextAreaElement)) submit(el);
    key(el, "Enter", "keyup");
  }}
  return JSON.stringify({{ desc: describe(el) }});
"#,
        r = lit(r#ref),
        t = lit(text),
        s = submit
    ))
}

/// Press `key` on whatever has focus (`browser_press`). Answers `{desc}`.
pub fn press(k: &str) -> String {
    script(&format!(
        r#"
  const k = {k};
  const el = document.activeElement && document.activeElement !== document.body ? document.activeElement : document.body;
  const go = key(el, k, "keydown");
  if (k.length === 1) key(el, k, "keypress");
  if (go) {{
    if (k.length === 1 && editable(el)) {{
      if (el.isContentEditable) document.execCommand("insertText", false, k);
      else setValue(el, el.value + k);
    }} else if (k === "Backspace" && editable(el) && !el.isContentEditable) {{
      setValue(el, el.value.slice(0, -1));
    }} else if (k === "Enter") {{
      if (el instanceof HTMLInputElement) submit(el);
      else if (el instanceof HTMLAnchorElement || el instanceof HTMLButtonElement || el.getAttribute("role") === "button") el.click();
    }} else if (k === " " && (el instanceof HTMLButtonElement || (el instanceof HTMLInputElement && ["checkbox", "radio"].includes(el.type)))) {{
      el.click();
    }} else if (k === "Tab") {{
      const all = Array.from(document.querySelectorAll('a[href],button,input,select,textarea,[tabindex]:not([tabindex="-1"])')).filter((x) => !x.disabled && x.getClientRects().length);
      const next = all[(all.indexOf(el) + 1) % Math.max(all.length, 1)];
      if (next) next.focus();
    }}
  }}
  key(el, k, "keyup");
  return JSON.stringify({{ desc: el === document.body ? "the page" : describe(el) }});
"#,
        k = lit(k)
    ))
}

/// The console messages kept since the page loaded (`browser_console`),
/// emptied when `clear`. Answers `{messages: [{level, text, at}]}`.
pub fn console(clear: bool) -> String {
    script(&format!(
        r#"
  const messages = fd.console.slice();
  if ({clear}) fd.console.length = 0;
  return JSON.stringify({{ messages }});
"#
    ))
}

/// Start `expression` as call `id` (`browser_eval`): answers `"started"`,
/// and the result waits for [`eval_poll`].
pub fn eval_start(id: u64, expression: &str) -> String {
    // A trailing `;` would end the parenthesised expression early.
    let expression = expression.trim().trim_end_matches(';');
    format!(
        r#"(() => {{
  const fd = window.__fd || (window.__fd = {{ console: [], refs: new Map() }});
  fd.evals = fd.evals || {{}};
  const show = (v) => {{
    try {{
      if (v === undefined) return "undefined";
      if (typeof Element !== "undefined" && v instanceof Element) return v.outerHTML.slice(0, 4000);
      const s = JSON.stringify(v, null, 2);
      return s === undefined ? String(v) : s;
    }} catch (e) {{ return String(v); }}
  }};
  Promise.resolve().then(() => {{
    const value = (
{expression}
    );
    return typeof value === "function" ? value() : value;
  }}).then(
    (v) => {{ fd.evals[{id}] = {{ ok: true, value: show(v) }}; }},
    (e) => {{
      const value = e && e.message !== undefined
        ? (e.name || "Error") + ": " + e.message + (e.stack ? "\n" + e.stack : "")
        : String(e);
      fd.evals[{id}] = {{ ok: false, value }};
    }}
  );
  return JSON.stringify("started");
}})()"#
    )
}

/// Call `id`'s result once there is one: `{ok, value}` or `null`.
pub fn eval_poll(id: u64) -> String {
    format!(
        r#"(() => {{
  const fd = window.__fd;
  const r = fd && fd.evals && fd.evals[{id}];
  if (!r) return JSON.stringify(null);
  delete fd.evals[{id}];
  return JSON.stringify(r);
}})()"#
    )
}

/// The value a script answered, from what wry's callback handed back: the
/// script's string, JSON-encoded once more. `None` when the script threw or
/// did not run.
pub fn decode(raw: &str) -> Option<serde_json::Value> {
    let inner: String = serde_json::from_str(raw).ok()?;
    serde_json::from_str(&inner).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_unwrapped_twice_and_a_failure_is_none() {
        let raw = serde_json::to_string(r#"{"desc":"button \"Go\""}"#).unwrap();
        assert_eq!(
            decode(&raw).unwrap()["desc"].as_str(),
            Some("button \"Go\"")
        );
        assert!(decode("").is_none());
        assert!(decode("null").is_none());
    }

    #[test]
    fn agent_values_are_quoted_never_pasted() {
        let js = click("e1\"); alert(1); (\"");
        assert!(js.contains(r#"byRef("e1\"); alert(1); (\"")"#));
        let js = type_text("e2", "</script>'\"`${x}`", true);
        assert!(js.contains(r#"const text = "</script>'\"`${x}`";"#));
        assert!(js.contains("if (true)"));
        assert!(press("Enter").contains(r#"const k = "Enter";"#));
    }

    #[test]
    fn an_expression_loses_its_trailing_semicolon() {
        let js = eval_start(3, "document.title;  ");
        assert!(js.contains("\ndocument.title\n"));
        assert!(js.contains("fd.evals[3]"));
        assert!(eval_poll(3).contains("fd.evals[3]"));
    }
}
