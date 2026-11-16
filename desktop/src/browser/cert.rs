//! What both platforms' halves of B21 (`specs/SESSION_BROWSER_PRD.md`) share:
//! a page that failed on its certificate, and the `host:port` a certificate
//! is trusted for (`tls_macos`, `tls_windows`).

/// A page that failed on its certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertFailure {
    /// The address that failed.
    pub url: String,
    pub host: String,
    pub port: u16,
    /// The web view's own words for what was wrong.
    pub message: String,
}

/// An `https://` address's host (lowercase, IPv6 unbracketed) and port.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("wss") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let (host, tail) = after.split_once(']')?;
        (host, tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(port) => port.parse().ok()?,
        None => 443,
    };
    Some((host.to_ascii_lowercase(), port))
}

/// Whether `failure` is the page's own, the address the browser is loading
/// or showing (`page`), rather than an image or a script on it: only the
/// page's gets our page with Continue anyway. WebView2 reports every
/// resource; WebKit only the page.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn is_page_failure(failure: &CertFailure, page: &str) -> bool {
    host_port(page).is_some_and(|(host, port)| host == failure.host && port == failure.port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_https_address_names_its_host_and_port() {
        assert_eq!(
            host_port("https://localhost:8443/a"),
            Some(("localhost".into(), 8443))
        );
        assert_eq!(
            host_port("https://Dev.Test/"),
            Some(("dev.test".into(), 443))
        );
        assert_eq!(host_port("https://[::1]:9443/"), Some(("::1".into(), 9443)));
        assert_eq!(
            host_port("wss://localhost:24678/hmr"),
            Some(("localhost".into(), 24678))
        );
        assert_eq!(host_port("http://localhost:3000"), None, "not TLS");
        assert_eq!(host_port(""), None);
    }

    #[test]
    fn only_the_pages_own_certificate_failure_is_shown() {
        let failure = CertFailure {
            url: "https://localhost:8443/app.js".into(),
            host: "localhost".into(),
            port: 8443,
            message: String::new(),
        };
        assert!(is_page_failure(&failure, "https://localhost:8443/"));
        assert!(is_page_failure(&failure, "https://LOCALHOST:8443/x"));
        assert!(!is_page_failure(&failure, "https://localhost:9443/"));
        assert!(!is_page_failure(&failure, "https://example.com/"));
        assert!(
            !is_page_failure(&failure, "http://localhost:8443/"),
            "not TLS"
        );
        assert!(!is_page_failure(&failure, ""));
    }
}
