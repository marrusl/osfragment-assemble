use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Longest accepted fragment name. Fragment names become directory names in
/// a build context and stage names in a Containerfile, neither of which has
/// a reason to be long; the cap exists so a pathological name cannot push a
/// generated path past a filesystem or builder limit.
const MAX_FRAGMENT_NAME_LEN: usize = 64;

/// A fragment name that is safe to use as a single path component.
///
/// Grammar, enforced by [`FragmentName::new`]:
///
/// ```text
/// [a-z0-9]([a-z0-9._-]*[a-z0-9])?     1 to 64 characters
/// ```
///
/// Lowercase ASCII letters and digits, optionally separated by `.`, `-`, or
/// `_`, starting and ending with a letter or digit. That rejects the empty
/// name, whitespace, `.` and `..`, any name containing a path separator, and
/// any absolute path, so joining one onto a directory can only ever produce a
/// child of that directory.
///
/// The inner `String` is private to this module, so the only way to obtain a
/// `FragmentName` is through `new`. Holding one is therefore proof that the
/// grammar was checked, which is what lets `src/self_contained.rs` and
/// `src/generator.rs` build paths out of it without rechecking at each join.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FragmentName(String);

impl FragmentName {
    /// Validate `name` against the fragment-name grammar.
    ///
    /// Rejects rather than sanitizes: silently rewriting a name into
    /// something safe would produce a build that does not match what the
    /// fragment author wrote.
    pub fn new(name: &str) -> Result<Self> {
        let bytes = name.as_bytes();
        let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        let well_formed = bytes.len() <= MAX_FRAGMENT_NAME_LEN
            && bytes.first().is_some_and(|&b| is_alnum(b))
            && bytes.last().is_some_and(|&b| is_alnum(b))
            && bytes
                .iter()
                .all(|&b| is_alnum(b) || matches!(b, b'.' | b'-' | b'_'));

        if !well_formed {
            bail!(
                "invalid fragment name '{}': a name must be 1 to {} characters of \
                 lowercase letters, digits, '.', '-', or '_', and must start and end \
                 with a letter or digit. The name becomes a directory name in the \
                 build context and a Containerfile stage name, so it is rejected \
                 rather than rewritten.",
                name.escape_debug(),
                MAX_FRAGMENT_NAME_LEN
            );
        }
        Ok(Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FragmentName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for FragmentName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Lets a validated name be joined onto a path directly. Safe precisely
/// because the grammar admits no separator, no `..`, and no leading `/`.
impl AsRef<Path> for FragmentName {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl PartialEq<&str> for FragmentName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FragmentProvides {
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FragmentPackages {
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FragmentConflicts {
    #[serde(default)]
    pub fragments: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FragmentToml {
    fragment: FragmentInner,
}

#[derive(Debug, Clone, Deserialize)]
struct FragmentInner {
    name: String,
    version: String,
    description: String,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    provides: FragmentProvides,
    #[serde(default)]
    packages: FragmentPackages,
    #[serde(default)]
    conflicts: FragmentConflicts,
}

#[derive(Debug, Clone)]
pub struct Fragment {
    pub name: FragmentName,
    pub version: String,
    pub description: String,
    pub vendor: Option<String>,
    pub provides: FragmentProvides,
    pub packages: FragmentPackages,
    pub conflicts: FragmentConflicts,
}

pub fn parse_fragment_toml(content: &str) -> Result<Fragment> {
    let parsed: FragmentToml = toml::from_str(content).context("failed to parse fragment.toml")?;
    let inner = parsed.fragment;
    Ok(Fragment {
        // The single validation point for names arriving from a fragment's
        // own metadata. Everything downstream receives a FragmentName and
        // cannot reconstruct an unchecked one.
        name: FragmentName::new(&inner.name)?,
        version: inner.version,
        description: inner.description,
        vendor: inner.vendor,
        provides: inner.provides,
        packages: inner.packages,
        conflicts: inner.conflicts,
    })
}

pub const REPO_PREFIXES: &[&str] = &["tree/etc/yum.repos.d/", "tree/etc/pki/rpm-gpg/"];

pub fn is_repo_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    REPO_PREFIXES.iter().any(|prefix| s.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[fragment]
name = "epel"
version = "10"
description = "EPEL repository for RHEL"

[fragment.provides]
repos = ["epel"]
"#;

    const FULL_TOML: &str = r#"
[fragment]
name = "tailscale"
version = "1.82.0"
description = "Tailscale VPN client"
vendor = "Tailscale Inc."

[fragment.provides]
repos = ["tailscale-stable"]

[fragment.packages]
required = ["tailscale"]

[fragment.conflicts]
fragments = []
"#;

    #[test]
    fn parse_minimal_fragment() {
        let frag = parse_fragment_toml(MINIMAL_TOML).unwrap();
        assert_eq!(frag.name, "epel");
        assert_eq!(frag.version, "10");
        assert_eq!(frag.provides.repos, vec!["epel"]);
        assert!(frag.vendor.is_none());
        assert!(frag.packages.required.is_empty());
    }

    #[test]
    fn parse_full_fragment() {
        let frag = parse_fragment_toml(FULL_TOML).unwrap();
        assert_eq!(frag.name, "tailscale");
        assert_eq!(frag.vendor.as_deref(), Some("Tailscale Inc."));
        assert_eq!(frag.packages.required, vec!["tailscale"]);
    }

    /// A fragment published before `phase` was removed still carries the key.
    /// `FragmentInner` does not deny unknown fields, so such a fragment parses
    /// and the stale key is ignored rather than rejected. Previously published
    /// fragments therefore keep working without a rebuild.
    ///
    /// The value is deliberately one the old schema would have rejected, so
    /// this test fails both if `deny_unknown_fields` is added and if `phase`
    /// is reintroduced as a typed field. It does not depend on a sibling test
    /// to catch either mutation.
    #[test]
    fn stale_phase_key_is_ignored() {
        let stale = r#"
[fragment]
name = "legacy"
version = "1"
description = "published before phase was removed"
phase = "install"
"#;
        let frag = parse_fragment_toml(stale).expect("stale phase key must not be rejected");
        assert_eq!(frag.name, "legacy");
    }

    #[test]
    fn reject_unknown_packages_field() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "unknown field test"

[fragment.packages]
available = ["grafana"]
"#;
        let err = parse_fragment_toml(bad).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' error, got: {}",
            msg
        );
    }

    #[test]
    fn reject_typo_in_packages_field() {
        let bad = r#"
[fragment]
name = "bad"
version = "1"
description = "typo field test"

[fragment.packages]
requred = ["grafana"]
"#;
        let err = parse_fragment_toml(bad).unwrap_err();
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' error, got: {}",
            msg
        );
    }

    /// The grammar is the whole of Fix 1: every path-unsafe name has to be
    /// refused here, because nothing downstream rechecks. The rejected list
    /// leads with the traversal and separator vectors that reached a
    /// filesystem join before this type existed.
    #[test]
    fn fragment_name_grammar() {
        let max_len = "a".repeat(MAX_FRAGMENT_NAME_LEN);
        let too_long = "a".repeat(MAX_FRAGMENT_NAME_LEN + 1);

        let accepted = [
            "epel",
            "cis-hardening",
            "nvidia-driver-run",
            "node_exporter",
            "postgresql17",
            "postgres.17",
            "a",
            "7",
            max_len.as_str(),
        ];
        let rejected = [
            // Traversal.
            "..",
            "../evil",
            "../../escape",
            "fragment/../../etc",
            // Path separators.
            "a/b",
            "epel/",
            "/epel",
            "a\\b",
            // Absolute paths.
            "/",
            "/etc/passwd",
            // Empty and whitespace.
            "",
            " ",
            "   ",
            "epel fragment",
            "epel\n",
            "\tepel",
            // Current directory and hidden names.
            ".",
            "./epel",
            ".hidden",
            // Leading and trailing separators.
            "-epel",
            "epel-",
            "_epel",
            "epel.",
            // Character set.
            "EPEL",
            "Epel",
            "naive\u{301}",
            "epel\0",
            // Length.
            too_long.as_str(),
        ];

        for name in accepted {
            assert!(
                FragmentName::new(name).is_ok(),
                "'{}' must be accepted",
                name.escape_debug()
            );
        }
        for name in rejected {
            assert!(
                FragmentName::new(name).is_err(),
                "'{}' must be rejected",
                name.escape_debug()
            );
        }
    }

    /// The property the rest of the codebase relies on: a validated name is
    /// exactly one path component, so joining it onto a directory yields a
    /// direct child of that directory. Loosening the grammar to admit a
    /// separator would fail here rather than silently re-opening the escape.
    #[test]
    fn a_validated_name_is_a_single_path_component() {
        for name in ["epel", "cis-hardening", "postgres.17", "node_exporter"] {
            let validated = FragmentName::new(name).unwrap();
            let joined = Path::new("fragments").join(&validated);
            assert_eq!(
                joined.components().count(),
                2,
                "{} did not stay a single component",
                joined.display()
            );
            assert_eq!(joined.parent(), Some(Path::new("fragments")));
        }
    }

    /// The name arriving from a fragment's own metadata is validated at the
    /// parse boundary, so an unsafe one never reaches a caller at all.
    #[test]
    fn parse_rejects_a_path_unsafe_name() {
        let bad = r#"
[fragment]
name = "../../escape"
version = "1"
description = "traversal in the name"
"#;
        let err = parse_fragment_toml(bad).unwrap_err().to_string();
        assert!(
            err.contains("invalid fragment name") && err.contains("../../escape"),
            "error must name the offending value, got: {err}"
        );
    }

    #[test]
    fn parse_all_example_fragments() {
        let examples_dir = Path::new("examples/fragments");
        if !examples_dir.exists() {
            return; // skip if not in repo root
        }
        for entry in std::fs::read_dir(examples_dir).unwrap() {
            let entry = entry.unwrap();
            let toml_path = entry.path().join("fragment.toml");
            if toml_path.exists() {
                let content = std::fs::read_to_string(&toml_path).unwrap();
                let result = parse_fragment_toml(&content);
                assert!(
                    result.is_ok(),
                    "Failed to parse {}: {}",
                    toml_path.display(),
                    result.unwrap_err()
                );
            }
        }
    }

    #[test]
    fn postgresql_example_declares_required_packages() {
        let toml_path = Path::new("examples/fragments/postgresql/fragment.toml");
        let content =
            std::fs::read_to_string(toml_path).expect("postgresql example fragment should exist");
        let frag = parse_fragment_toml(&content).expect("postgresql example fragment should parse");
        assert!(
            frag.packages
                .required
                .contains(&"postgresql17-server".to_string()),
            "postgresql must declare postgresql17-server as required"
        );
        assert!(
            frag.packages.required.contains(&"postgresql17".to_string()),
            "postgresql must declare postgresql17 as required"
        );
    }
}
