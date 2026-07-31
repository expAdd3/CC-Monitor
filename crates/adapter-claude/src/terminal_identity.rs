//! Pure terminal identity catalog shared by collection and desktop adapters.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalIdentity {
    canonical_bundle_id: &'static str,
    application_path: &'static str,
}

impl TerminalIdentity {
    pub const fn canonical_bundle_id(self) -> &'static str {
        self.canonical_bundle_id
    }

    pub const fn application_path(self) -> &'static str {
        self.application_path
    }
}

struct TerminalRecord {
    identity: TerminalIdentity,
    term_program_aliases: &'static [&'static str],
    bundle_id_aliases: &'static [&'static str],
}

const TERMINALS: &[TerminalRecord] = &[
    TerminalRecord {
        identity: TerminalIdentity {
            canonical_bundle_id: "com.apple.Terminal",
            application_path: "/System/Applications/Utilities/Terminal.app",
        },
        term_program_aliases: &["Apple_Terminal", "Terminal.app"],
        bundle_id_aliases: &["com.apple.Terminal"],
    },
    TerminalRecord {
        identity: TerminalIdentity {
            canonical_bundle_id: "com.googlecode.iterm2",
            application_path: "/Applications/iTerm.app",
        },
        term_program_aliases: &["iTerm.app", "iTerm2"],
        bundle_id_aliases: &["com.googlecode.iterm2"],
    },
    TerminalRecord {
        identity: TerminalIdentity {
            canonical_bundle_id: "dev.warp.Warp-Stable",
            application_path: "/Applications/Warp.app",
        },
        term_program_aliases: &["WarpTerminal", "Warp"],
        bundle_id_aliases: &["dev.warp.Warp-Stable", "dev.warp.Warp"],
    },
    TerminalRecord {
        identity: TerminalIdentity {
            canonical_bundle_id: "com.microsoft.VSCode",
            application_path: "/Applications/Visual Studio Code.app",
        },
        term_program_aliases: &["vscode"],
        bundle_id_aliases: &["com.microsoft.VSCode"],
    },
];

pub fn for_term_program(value: &str) -> Option<TerminalIdentity> {
    find(value, |record| record.term_program_aliases)
}

pub fn for_bundle_id(value: &str) -> Option<TerminalIdentity> {
    find(value, |record| record.bundle_id_aliases)
}

fn find(
    value: &str,
    aliases: impl Fn(&TerminalRecord) -> &'static [&'static str],
) -> Option<TerminalIdentity> {
    let value = value.trim();
    TERMINALS
        .iter()
        .find(|record| {
            aliases(record)
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(value))
        })
        .map(|record| record.identity)
}

#[cfg(test)]
mod tests {
    use super::{for_bundle_id, for_term_program};

    #[test]
    fn term_program_aliases_resolve_to_canonical_identities() {
        let cases = [
            ("Apple_Terminal", "com.apple.Terminal"),
            ("Terminal.app", "com.apple.Terminal"),
            ("iTerm.app", "com.googlecode.iterm2"),
            ("iTerm2", "com.googlecode.iterm2"),
            ("vscode", "com.microsoft.VSCode"),
        ];
        for (alias, expected) in cases {
            assert_eq!(
                for_term_program(alias).map(|identity| identity.canonical_bundle_id()),
                Some(expected),
                "{alias}"
            );
        }
        assert_eq!(for_term_program("unknown"), None);
    }

    #[test]
    fn warp_aliases_share_one_canonical_bundle_and_fallback_path() {
        for alias in ["WarpTerminal", "Warp"] {
            let identity = for_term_program(alias).expect("known TERM_PROGRAM alias");
            assert_eq!(identity.canonical_bundle_id(), "dev.warp.Warp-Stable");
            assert_eq!(identity.application_path(), "/Applications/Warp.app");
        }
        for alias in ["dev.warp.Warp-Stable", "dev.warp.Warp"] {
            let identity = for_bundle_id(alias).expect("known Warp bundle alias");
            assert_eq!(identity.canonical_bundle_id(), "dev.warp.Warp-Stable");
            assert_eq!(identity.application_path(), "/Applications/Warp.app");
        }
    }

    #[test]
    fn bundle_lookup_is_an_explicit_allowlist_with_known_paths() {
        let terminal = for_bundle_id("com.apple.Terminal").expect("allowlisted Terminal");
        assert_eq!(
            terminal.application_path(),
            "/System/Applications/Utilities/Terminal.app"
        );
        let iterm = for_bundle_id("com.googlecode.iterm2").expect("allowlisted iTerm");
        assert_eq!(iterm.application_path(), "/Applications/iTerm.app");
        let vscode = for_bundle_id("com.microsoft.VSCode").expect("allowlisted VS Code");
        assert_eq!(
            vscode.application_path(),
            "/Applications/Visual Studio Code.app"
        );
        assert_eq!(for_bundle_id("com.example.untrusted"), None);
    }

    #[test]
    fn aliases_ignore_surrounding_space_and_ascii_case() {
        assert_eq!(
            for_term_program("  warpterminal  ").map(|identity| identity.canonical_bundle_id()),
            Some("dev.warp.Warp-Stable")
        );
        assert_eq!(
            for_bundle_id("DEV.WARP.WARP").map(|identity| identity.canonical_bundle_id()),
            Some("dev.warp.Warp-Stable")
        );
    }
}
