//! The `webui/` SPA, baked into the binary (D9, `specs/WEB_INTERFACE.md`).
//!
//! `vite build` writes static assets to `webui/dist/`; [`rust_embed`] embeds
//! that folder's *contents* into the binary at compile time (release builds)
//! or reads them straight off disk on each call (plain debug builds — see
//! [`rust_embed::RustEmbed::get`]), so the module works the same way in
//! `cargo run` and in a released binary. Either way the server "never
//! resolves paths on disk" from a caller's point of view (D9): this is the
//! only place that knows `webui/dist/` exists.
//!
//! `#[folder = "webui/dist/"]` is resolved relative to `CARGO_MANIFEST_DIR`
//! by the derive macro, so it compiles regardless of the crate's current
//! working directory. It must resolve to *some* folder even before any `npm
//! run build` has happened, because `rust-embed` walks it at compile time —
//! that is exactly what the tracked `webui/dist/.gitkeep` is for. Everything
//! else `vite build` writes into `webui/dist/` is gitignored (see the repo's
//! `.gitignore`), so a clean checkout compiles with an "empty" SPA and this
//! module reports that honestly via [`Lookup::NotBuilt`] rather than serving
//! a blank page or panicking.
//!
//! [`lookup`] is the API `src/web/server.rs` (a separate task, D6) builds its
//! asset route on: give it a request path, get back bytes + a content type,
//! or an explicit reason there is nothing to serve.

use std::borrow::Cow;

/// The built SPA. Kept private: nothing outside this module should reach for
/// `Assets::get` directly, so the fallback/not-built rules in [`lookup`] are
/// the only way in.
#[derive(rust_embed::Embed)]
#[folder = "webui/dist/"]
struct Assets;

/// One servable asset: its bytes and the content type to send with them.
///
/// Both fields are `Cow` because an embedded (release) asset borrows
/// `'static` bytes/metadata baked into the binary, while a plain debug build
/// or [`not_built_page`]'s literal HTML instead owns freshly-allocated data —
/// callers (`server.rs`) don't need to care which.
#[derive(Clone)]
pub struct Asset {
    pub body: Cow<'static, [u8]>,
    pub content_type: Cow<'static, str>,
}

impl From<rust_embed::EmbeddedFile> for Asset {
    fn from(file: rust_embed::EmbeddedFile) -> Self {
        // `metadata.mimetype()` borrows from `file.metadata`, which we are
        // about to drop, so it must be copied out rather than held as a
        // reference (see the `mime-guess` feature enabled on `rust-embed` in
        // Cargo.toml).
        let content_type = file.metadata.mimetype().to_string();
        Asset {
            body: file.data,
            content_type: Cow::Owned(content_type),
        }
    }
}

/// The result of resolving a request path against the embedded SPA.
pub enum Lookup {
    /// The exact asset, or `index.html` substituted for an unmatched
    /// client-side route — see [`lookup`]'s fallback rule.
    Found(Asset),
    /// `path` looked like a concrete asset request (its last path segment
    /// contains a `.`, e.g. `/assets/app-xyz.js`) and no such file is
    /// embedded. A genuine 404 — distinct from [`Lookup::NotBuilt`], which
    /// means nothing was ever built at all.
    NotFound,
    /// `webui/dist/` holds no built SPA: a clean checkout with `npm run
    /// build` never run (only `webui/dist/.gitkeep` present), or a release
    /// that shipped without the frontend step. Every path resolves to this,
    /// including `/`. Callers must render [`not_built_page`] rather than
    /// treat this as a 404 or serve nothing — a blank tab with no
    /// explanation is exactly what this variant exists to rule out.
    NotBuilt,
}

/// Resolve `request_path` (e.g. an axum request's `uri().path()`) against the
/// embedded SPA.
///
/// Rules, in order:
/// 1. If the SPA was never built — `index.html` is missing from the embed,
///    which is exactly what a fresh checkout with only `webui/dist/.gitkeep`
///    produces — every path returns [`Lookup::NotBuilt`].
/// 2. A leading `/` is stripped; an empty path (`""` or `"/"`) is treated as
///    `index.html`.
/// 3. An exact embedded match is returned as [`Lookup::Found`].
/// 4. Otherwise: if the path's last segment contains a `.` (it looks like a
///    real asset request — JS/CSS/font/etc.), it is a genuine
///    [`Lookup::NotFound`]. If it does not (an extensionless client-side
///    route the SPA's own router owns, e.g. `/session/42`), this falls back
///    to `index.html` — the standard SPA convention, so reloading deep
///    inside the app doesn't 404.
pub fn lookup(request_path: &str) -> Lookup {
    resolve(&EmbeddedAssets, request_path)
}

/// A small, self-contained explanatory page for [`Lookup::NotBuilt`]. Plain
/// HTML with inline styles, so it needs no other embedded asset (font, CSS)
/// to render — the whole point is that it works when nothing else does.
pub fn not_built_page() -> Asset {
    const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>FlightDeck Web — not built</title>
</head>
<body style="font-family: ui-monospace, monospace; background: #04090f; color: #edf4ff; padding: 3rem; line-height: 1.6;">
<h1 style="font-weight: 500;">webui was not built</h1>
<p>This FlightDeck binary was compiled without a <code>webui/</code> build, so
there is no browser control surface to serve.</p>
<p>Run <code>npm run build</code> in <code>webui/</code>, then rebuild
FlightDeck.</p>
</body>
</html>"#;

    Asset {
        body: Cow::Borrowed(HTML.as_bytes()),
        content_type: Cow::Borrowed("text/html; charset=utf-8"),
    }
}

/// Abstracts "does this path resolve to an asset", so the fallback/not-built
/// decision logic in [`resolve`] is unit-testable against a small in-memory
/// fixture (see the `tests` module) instead of the real `webui/dist/`, whose
/// contents depend on whether `npm run build` happened to run before `cargo
/// test` in this invocation of the ship gate — the four scenarios the tests
/// must cover ("exists", "does not exist", "SPA fallback", "nothing built")
/// need to be exercised deterministically regardless of that ordering.
trait AssetSource {
    fn get(&self, path: &str) -> Option<Asset>;
}

struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn get(&self, path: &str) -> Option<Asset> {
        Assets::get(path).map(Asset::from)
    }
}

fn resolve(source: &impl AssetSource, request_path: &str) -> Lookup {
    // `index.html`'s presence is the one signal for "was this ever built" —
    // fetched once and reused below as the SPA fallback, rather than
    // re-fetched, so the not-built check and the fallback never disagree.
    let Some(index) = source.get("index.html") else {
        return Lookup::NotBuilt;
    };

    let rel_path = normalize(request_path);

    if let Some(asset) = source.get(&rel_path) {
        return Lookup::Found(asset);
    }

    if looks_like_asset_request(&rel_path) {
        return Lookup::NotFound;
    }

    Lookup::Found(index)
}

fn normalize(request_path: &str) -> String {
    let trimmed = request_path.trim_start_matches('/');
    if trimmed.is_empty() {
        "index.html".to_string()
    } else {
        trimmed.to_string()
    }
}

fn looks_like_asset_request(rel_path: &str) -> bool {
    rel_path
        .rsplit('/')
        .next()
        .is_some_and(|last_segment| last_segment.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn asset(body: &'static str, content_type: &'static str) -> Asset {
        Asset {
            body: Cow::Borrowed(body.as_bytes()),
            content_type: Cow::Borrowed(content_type),
        }
    }

    /// An in-memory stand-in for the embedded `webui/dist/`, so these tests
    /// exercise `resolve`'s decision logic without depending on whether `npm
    /// run build` has actually run in this checkout.
    struct FakeAssets(HashMap<&'static str, Asset>);

    impl AssetSource for FakeAssets {
        fn get(&self, path: &str) -> Option<Asset> {
            self.0.get(path).cloned()
        }
    }

    fn built_fixture() -> FakeAssets {
        let mut files = HashMap::new();
        files.insert("index.html", asset("<html>shell</html>", "text/html"));
        files.insert(
            "assets/app.js",
            asset("console.log('hi')", "text/javascript"),
        );
        FakeAssets(files)
    }

    fn empty_fixture() -> FakeAssets {
        FakeAssets(HashMap::new())
    }

    #[test]
    fn an_asset_that_exists_is_found_verbatim() {
        match resolve(&built_fixture(), "/assets/app.js") {
            Lookup::Found(asset) => {
                assert_eq!(&*asset.body, b"console.log('hi')");
                assert_eq!(asset.content_type, "text/javascript");
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn an_asset_that_does_not_exist_is_a_real_404() {
        // Has a file extension, so this is a genuine missing-asset request,
        // not a client-side route the SPA fallback should own.
        match resolve(&built_fixture(), "/assets/missing.js") {
            Lookup::NotFound => {}
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn an_extensionless_route_falls_back_to_index_html() {
        // No file extension on the last segment: this is a client-side
        // route (e.g. the SPA's own router), so a reload must not 404.
        match resolve(&built_fixture(), "/session/42") {
            Lookup::Found(asset) => {
                assert_eq!(&*asset.body, b"<html>shell</html>");
            }
            _ => panic!("expected the SPA fallback to serve index.html"),
        }
    }

    #[test]
    fn root_path_serves_index_html() {
        match resolve(&built_fixture(), "/") {
            Lookup::Found(asset) => assert_eq!(&*asset.body, b"<html>shell</html>"),
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn nothing_built_reports_not_built_for_every_path() {
        for path in ["/", "/index.html", "/assets/app.js", "/session/42"] {
            match resolve(&empty_fixture(), path) {
                Lookup::NotBuilt => {}
                _ => panic!("expected NotBuilt for {path}"),
            }
        }
    }

    #[test]
    fn not_built_page_is_non_empty_html_and_never_panics() {
        let page = not_built_page();
        assert!(page.content_type.starts_with("text/html"));
        assert!(!page.body.is_empty());
    }

    /// A light smoke test against the *real* embedded `Assets`, to prove the
    /// `#[folder = "webui/dist/"]` wiring itself compiles and resolves to
    /// something sane — without assuming whether `npm run build` has run
    /// before this test does (see the `AssetSource` doc comment above for
    /// why the behavioural tests above use a fixture instead).
    #[test]
    fn real_embed_lookup_never_panics_on_root() {
        match lookup("/") {
            Lookup::Found(_) | Lookup::NotBuilt => {}
            Lookup::NotFound => panic!("root path should never be a plain 404"),
        }
    }
}
