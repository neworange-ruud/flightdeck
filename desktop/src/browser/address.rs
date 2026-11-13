//! What the session browser's address field means (`specs/SESSION_BROWSER_PRD.md`
//! B8, B12, B22, B24): text typed into it turned into a URL to load, or why
//! not, and which URLs reach a loopback port that the relay must forward
//! first.

/// What the address field's text asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    /// Load this URL.
    Load(String),
    /// Refused, in words for the field's note.
    Refused(String),
}

/// Why `file://` is refused (B12): it would read this computer's files, not
/// the worker's.
pub const FILE_REFUSED: &str =
    "file:// pages are not shown here: they would be this computer's files, not the worker's.";

/// Why other schemes are refused.
pub const SCHEME_REFUSED: &str = "Only http:// and https:// addresses are shown here.";

/// Why text that is no address is refused: the field does not search.
pub const NOT_AN_ADDRESS: &str =
    "That is not an address. Type one like localhost:3000 or example.com.";

/// Read what was typed:
///
/// - `http://…` and `https://…` load as they are;
/// - `file:` and every other scheme are refused;
/// - a bare `localhost[:port]`, an IPv4 or bracketed IPv6 address, or
///   anything with a port gets `http://` (a dev server is plain HTTP);
/// - a bare name with a dot (`example.com/path`) gets `https://`;
/// - anything else — spaces, a lone word — is not an address.
pub fn parse(text: &str) -> Address {
    let text = text.trim();
    if text.is_empty() {
        return Address::Refused(NOT_AN_ADDRESS.to_string());
    }
    if let Some((scheme, _)) = text.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => Address::Load(text.to_string()),
            "file" => Address::Refused(FILE_REFUSED.to_string()),
            _ => Address::Refused(SCHEME_REFUSED.to_string()),
        };
    }
    if text.to_ascii_lowercase().starts_with("file:") {
        return Address::Refused(FILE_REFUSED.to_string());
    }
    if text.chars().any(char::is_whitespace) {
        return Address::Refused(NOT_AN_ADDRESS.to_string());
    }
    let host_port = text.split(['/', '?', '#']).next().unwrap_or_default();
    let (host, has_port) = split_port(host_port);
    let host_lower = host.to_ascii_lowercase();
    let loopbackish = host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host.starts_with('[')
        || host.parse::<std::net::Ipv4Addr>().is_ok();
    if loopbackish || has_port {
        return Address::Load(format!("http://{text}"));
    }
    if host.contains('.') && !host.starts_with('.') && !host.ends_with('.') {
        return Address::Load(format!("https://{text}"));
    }
    Address::Refused(NOT_AN_ADDRESS.to_string())
}

/// `host` and whether a numeric port followed it.
fn split_port(host_port: &str) -> (&str, bool) {
    if let Some(rest) = host_port.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((_, after)) => (
                &host_port[..host_port.len() - after.len()],
                after.strip_prefix(':').is_some_and(is_port),
            ),
            None => (host_port, false),
        };
    }
    match host_port.rsplit_once(':') {
        Some((host, port)) if is_port(port) => (host, true),
        _ => (host_port, false),
    }
}

fn is_port(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && text.parse::<u16>().is_ok()
}

/// The loopback port `url` reaches, when it reaches one: `localhost`,
/// `*.localhost`, `127.0.0.1` or `[::1]`, with its explicit port or the
/// scheme's default. WKWebView never proxies these (B22), so the relay
/// forwards the port before the page loads (B24).
pub fn loopback_port(url: &str) -> Option<u16> {
    let (scheme, rest) = url.split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    // Drop any `user:pass@`.
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
    let host = host.to_ascii_lowercase();
    let loopback =
        host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "::1";
    if !loopback {
        return None;
    }
    match port {
        Some(port) => port.parse().ok().filter(|p| *p != 0),
        None => Some(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(url: &str) -> Address {
        Address::Load(url.to_string())
    }

    #[test]
    fn full_urls_load_as_typed() {
        assert_eq!(
            parse("http://localhost:3000/a"),
            load("http://localhost:3000/a")
        );
        assert_eq!(parse(" https://example.com "), load("https://example.com"));
        assert_eq!(parse("HTTPS://Example.com"), load("HTTPS://Example.com"));
    }

    #[test]
    fn a_dev_server_address_gets_http() {
        assert_eq!(parse("localhost"), load("http://localhost"));
        assert_eq!(parse("localhost:3000"), load("http://localhost:3000"));
        assert_eq!(
            parse("localhost:3000/login?x=1"),
            load("http://localhost:3000/login?x=1")
        );
        assert_eq!(
            parse("app.localhost:5173"),
            load("http://app.localhost:5173")
        );
        assert_eq!(parse("127.0.0.1:8000"), load("http://127.0.0.1:8000"));
        assert_eq!(parse("[::1]:8000"), load("http://[::1]:8000"));
        assert_eq!(parse("intranet:8080"), load("http://intranet:8080"));
    }

    #[test]
    fn a_name_with_a_dot_gets_https() {
        assert_eq!(parse("example.com"), load("https://example.com"));
        assert_eq!(parse("docs.rs/wry"), load("https://docs.rs/wry"));
    }

    #[test]
    fn files_other_schemes_and_searches_are_refused() {
        assert_eq!(
            parse("file:///etc/passwd"),
            Address::Refused(FILE_REFUSED.into())
        );
        assert_eq!(
            parse("file:/etc/passwd"),
            Address::Refused(FILE_REFUSED.into())
        );
        assert_eq!(
            parse("ftp://host/x"),
            Address::Refused(SCHEME_REFUSED.into())
        );
        for text in ["", "   ", "how do i", "intranet", ".com", "example."] {
            assert_eq!(
                parse(text),
                Address::Refused(NOT_AN_ADDRESS.into()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn loopback_ports_are_found_with_defaults() {
        assert_eq!(loopback_port("http://localhost:3000/x"), Some(3000));
        assert_eq!(loopback_port("http://localhost/"), Some(80));
        assert_eq!(loopback_port("https://localhost"), Some(443));
        assert_eq!(loopback_port("http://127.0.0.1:8000"), Some(8000));
        assert_eq!(loopback_port("http://[::1]:5173/"), Some(5173));
        assert_eq!(loopback_port("ws://localhost:24678/hmr"), Some(24678));
        assert_eq!(loopback_port("http://app.LOCALHOST:5173"), Some(5173));
        assert_eq!(loopback_port("http://user:pw@localhost:9000"), Some(9000));
    }

    #[test]
    fn other_hosts_need_no_forward() {
        assert_eq!(loopback_port("https://example.com"), None);
        assert_eq!(loopback_port("http://127.0.0.2:80"), None);
        assert_eq!(loopback_port("http://localhost.example.com:80"), None);
        assert_eq!(loopback_port("about:blank"), None);
        assert_eq!(loopback_port("file:///tmp"), None);
        assert_eq!(loopback_port("http://localhost:0"), None);
    }
}
