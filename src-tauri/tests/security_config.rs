use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const ALLOWED_DIRECTIVES: [&str; 9] = [
    "default-src",
    "connect-src",
    "img-src",
    "style-src",
    "script-src",
    "object-src",
    "base-uri",
    "frame-ancestors",
    "form-action",
];

fn directives(csp: &str) -> BTreeMap<&str, Vec<&str>> {
    let mut parsed = BTreeMap::new();
    for directive in csp
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let mut tokens = directive.split_whitespace();
        let name = tokens.next().expect("directive name");
        assert!(
            parsed.insert(name, tokens.collect()).is_none(),
            "duplicate CSP directive: {name}"
        );
    }
    parsed
}

fn is_nonce_or_hash(source: &str) -> bool {
    let Some(quoted) = source
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    else {
        return false;
    };
    let payload = ["nonce-", "sha256-", "sha384-", "sha512-"]
        .into_iter()
        .find_map(|prefix| quoted.strip_prefix(prefix));
    payload.is_some_and(|value| {
        !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
            })
    })
}

fn assert_only_allowed_sources(csp: &str, allowed_network: &[(&str, &str)]) {
    let allowed_directives = ALLOWED_DIRECTIVES.into_iter().collect::<BTreeSet<_>>();
    let allowed_network = allowed_network.iter().copied().collect::<BTreeSet<_>>();
    for (directive, sources) in directives(csp) {
        assert!(
            allowed_directives.contains(directive),
            "CSP directive is not allowlisted: {directive}"
        );
        if sources.contains(&"'none'") {
            assert_eq!(
                sources,
                vec!["'none'"],
                "'none' must be the directive's only source: {directive}"
            );
        }
        for source in sources {
            let allowed = match source {
                "'self'" | "'none'" => true,
                "'unsafe-inline'" => directive == "style-src",
                "ipc:" => directive == "connect-src",
                "data:" => directive == "img-src",
                _ if is_nonce_or_hash(source) => {
                    matches!(directive, "script-src" | "style-src")
                }
                _ => allowed_network.contains(&(directive, source)),
            };
            assert!(
                allowed,
                "CSP source is not allowlisted for {directive}: {source}"
            );
        }
    }
}

fn assert_safe_script_sources(csp: &str) {
    let parsed = directives(csp);
    assert_eq!(parsed.get("script-src"), Some(&vec!["'self'"]));
    for forbidden in [
        "'unsafe-inline'",
        "'unsafe-eval'",
        "'strict-dynamic'",
        "data:",
        "blob:",
        "*",
    ] {
        assert!(
            !parsed
                .get("script-src")
                .is_some_and(|sources| sources.contains(&forbidden)),
            "dangerous script source: {forbidden}"
        );
    }
}

#[test]
fn bundled_webview_has_a_restrictive_local_only_csp() {
    let config: Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid Tauri config");
    let csp = config["app"]["security"]["csp"]
        .as_str()
        .expect("CSP must be enabled");

    let production = directives(csp);
    assert_eq!(production.get("default-src"), Some(&vec!["'self'"]));
    assert_eq!(
        production.get("connect-src"),
        Some(&vec!["'self'", "ipc:", "http://ipc.localhost"])
    );
    assert_eq!(production.get("img-src"), Some(&vec!["'self'", "data:"]));
    assert_eq!(production.get("object-src"), Some(&vec!["'none'"]));
    assert_eq!(production.get("base-uri"), Some(&vec!["'none'"]));
    assert_eq!(production.get("frame-ancestors"), Some(&vec!["'none'"]));
    assert_eq!(production.get("form-action"), Some(&vec!["'none'"]));
    assert_only_allowed_sources(csp, &[("connect-src", "http://ipc.localhost")]);
    assert_safe_script_sources(csp);

    let dev_csp = config["app"]["security"]["devCsp"]
        .as_str()
        .expect("development CSP must be explicit");
    let development = directives(dev_csp);
    assert_eq!(
        development.get("connect-src"),
        Some(&vec![
            "'self'",
            "ipc:",
            "http://ipc.localhost",
            "ws://localhost:1420"
        ])
    );
    assert_only_allowed_sources(
        dev_csp,
        &[
            ("connect-src", "http://ipc.localhost"),
            ("connect-src", "ws://localhost:1420"),
        ],
    );
    assert_safe_script_sources(dev_csp);
}

#[test]
fn csp_contract_rejects_network_source_spelling_bypasses() {
    for source in [
        "*",
        "//evil.example",
        "evil.example",
        "*.evil.example",
        "https:",
        "https://evil.example",
    ] {
        let csp = format!("default-src 'self'; img-src 'self' {source}");
        assert!(
            std::panic::catch_unwind(|| assert_only_allowed_sources(&csp, &[])).is_err(),
            "network source spelling bypassed the allowlist: {source}"
        );
    }
}

#[test]
fn csp_contract_allows_scoped_local_sources_nonces_and_hashes() {
    assert_only_allowed_sources(
        "default-src 'self'; \
         connect-src 'self' ipc: http://ipc.localhost; \
         img-src 'self' data:; \
         style-src 'self' 'unsafe-inline' 'sha256-YWJjZA=='; \
         script-src 'self' 'nonce-YWJjZA=='",
        &[("connect-src", "http://ipc.localhost")],
    );
}
