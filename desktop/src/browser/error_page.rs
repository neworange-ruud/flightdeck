//! The session browser's own error page (`specs/SESSION_BROWSER_PRD.md` §5):
//! what the worker or the relay could not do, in its words, instead of the
//! engine's generic "cannot connect" page — which on a remote session would
//! not even say which machine failed.

/// A self-contained page: `title`, then `detail`, then which machine the
/// browser's network is (`via`, a remote session's worker).
pub fn page(title: &str, detail: &str, via: Option<&str>) -> String {
    let via = via
        .map(|host| {
            format!(
                "<p class=via>This browser's requests are made by <b>{}</b>.</p>",
                escape(host)
            )
        })
        .unwrap_or_default();
    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>{title}</title><style>\
         :root{{color-scheme:light dark;--ink:#1f2328;--muted:#59636e;--bg:#ffffff}}\
         @media (prefers-color-scheme:dark){{:root{{--ink:#e6edf3;--muted:#9198a1;--bg:#0d1117}}}}\
         body{{margin:0;background:var(--bg);color:var(--ink);\
         font:15px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',system-ui,sans-serif}}\
         main{{max-width:560px;margin:14vh auto;padding:0 24px}}\
         h1{{font-size:20px;font-weight:600;margin:0 0 8px}}\
         p{{margin:0 0 8px;color:var(--muted)}}.via{{font-size:13px;margin-top:20px}}\
         </style></head><body><main><h1>{title}</h1><p>{detail}</p>{via}</main></body></html>",
        title = escape(title),
        detail = escape(detail),
    )
}

/// The message our pages post when the person chooses their button
/// (`window.ipc.postMessage`, `wry`'s channel back to the view).
pub const CONTINUE_MESSAGE: &str = "flightdeck:continue";

/// [`page`], with one button that posts [`CONTINUE_MESSAGE`]: the
/// certificate page's **Continue anyway** (B21).
pub fn page_with_action(title: &str, detail: &str, via: Option<&str>, action: &str) -> String {
    let button = format!(
        "<p style='margin-top:20px'><button onclick=\"window.ipc.postMessage('{CONTINUE_MESSAGE}')\" \
         style='font:inherit;padding:6px 14px;border-radius:6px;border:1px solid var(--muted);\
         background:transparent;color:var(--ink);cursor:pointer'>{}</button></p>",
        escape(action)
    );
    page(title, detail, via).replacen("</main>", &format!("{button}</main>"), 1)
}

/// HTML-escape text for an element or attribute.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_says_what_failed_and_where_and_escapes_it() {
        let html = page(
            "Can't reach localhost:3000",
            "Nothing is listening on <localhost:3000> on the worker.",
            Some("studio & co"),
        );
        assert!(
            html.contains("<h1>Can&#39;t reach localhost:3000</h1>"),
            "{html}"
        );
        assert!(html.contains("&lt;localhost:3000&gt;"));
        assert!(html.contains("<b>studio &amp; co</b>"));
        assert!(!html.contains("<localhost"));
        assert!(!page("t", "d", None).contains("class=via"));
    }

    #[test]
    fn the_certificate_page_has_one_button_that_posts_continue() {
        let html = page_with_action("Untrusted", "<self-signed>", None, "Continue <anyway>");
        assert_eq!(html.matches("<button").count(), 1);
        assert!(html.contains(CONTINUE_MESSAGE));
        assert!(html.contains("Continue &lt;anyway&gt;"));
        assert!(html.contains("&lt;self-signed&gt;"));
    }
}
