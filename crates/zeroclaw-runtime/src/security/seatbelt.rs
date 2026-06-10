//! macOS sandbox-exec (Seatbelt) sandbox backend.
//!
//! Uses Apple's built-in `sandbox-exec` tool to enforce per-session Seatbelt
//! profiles that restrict network access, filesystem writes, and process
//! spawning. Policy files are generated in `.sb` format and written to a
//! temporary directory that is cleaned up when the sandbox is dropped.

use crate::security::traits::Sandbox;
use std::path::{Path, PathBuf};
use std::process::Command;

/// macOS sandbox-exec (Seatbelt) sandbox backend.
///
/// Generates per-session `.sb` policy files and wraps commands with
/// `sandbox-exec -f <policy>`. The policy denies network and filesystem
/// writes by default, allowing only the workspace directory.
#[derive(Debug, Clone)]
pub struct SeatbeltSandbox {
    /// Directory where per-session policy files are stored.
    policy_dir: PathBuf,
    /// Path to the generated policy file for this session.
    policy_path: PathBuf,
}

impl SeatbeltSandbox {
    /// Create a new Seatbelt sandbox, generating a per-session policy file.
    ///
    /// Returns an error if `sandbox-exec` is not available or the policy file
    /// cannot be written.
    pub fn new() -> std::io::Result<Self> {
        Self::with_workspace_and_outbound(None, &[])
    }

    /// Create a new Seatbelt sandbox for the provided workspace root.
    ///
    /// If no workspace is provided, falls back to the process current
    /// directory for compatibility with direct construction.
    pub fn with_workspace(workspace: Option<&Path>) -> std::io::Result<Self> {
        Self::with_workspace_and_outbound(workspace, &[])
    }

    /// Create a new Seatbelt sandbox for the provided workspace root and
    /// a list of additional outbound network hosts to allow.
    ///
    /// `outbound_allow` is rendered as `(allow network-outbound (remote tcp
    /// "host:port"))` rules appended to the policy's network section. Each
    /// entry must already be in `host:port` form (e.g.
    /// `aiordering.kfc.com.cn:443`); the caller is responsible for
    /// validating entries. Empty slice keeps the default localhost-only
    /// outbound.
    pub fn with_workspace_and_outbound(
        workspace: Option<&Path>,
        outbound_allow: &[String],
    ) -> std::io::Result<Self> {
        if !Self::is_installed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "sandbox-exec not found (requires macOS)",
            ));
        }

        let policy_dir = std::env::temp_dir().join("zeroclaw-seatbelt");
        std::fs::create_dir_all(&policy_dir)?;

        let session_id = uuid::Uuid::new_v4();
        let policy_path = policy_dir.join(format!("{session_id}.sb"));

        let workspace = workspace
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")));
        let policy = generate_policy(&workspace, outbound_allow);
        std::fs::write(&policy_path, &policy)?;

        Ok(Self {
            policy_dir,
            policy_path,
        })
    }

    /// Probe if sandbox-exec is available (for auto-detection).
    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    /// Check if `sandbox-exec` is available on this system.
    fn is_installed() -> bool {
        // sandbox-exec is a built-in macOS binary at /usr/bin/sandbox-exec
        Path::new("/usr/bin/sandbox-exec").exists()
            || Command::new("sandbox-exec")
                .arg("-n")
                .arg("no-network")
                .arg("true")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }

    /// Return the path to the generated policy file.
    pub fn policy_path(&self) -> &Path {
        &self.policy_path
    }

    /// Return the policy directory path.
    pub fn policy_dir(&self) -> &Path {
        &self.policy_dir
    }
}

impl Drop for SeatbeltSandbox {
    fn drop(&mut self) {
        // Clean up the per-session policy file
        let _ = std::fs::remove_file(&self.policy_path);
    }
}

impl Sandbox for SeatbeltSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut sandbox_cmd = Command::new("sandbox-exec");
        sandbox_cmd.arg("-f");
        sandbox_cmd.arg(&self.policy_path);
        sandbox_cmd.arg(&program);
        sandbox_cmd.args(&args);

        *cmd = sandbox_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        Self::is_installed() && self.policy_path.exists()
    }

    fn name(&self) -> &str {
        "sandbox-exec"
    }

    fn description(&self) -> &str {
        "macOS Seatbelt sandbox (built-in sandbox-exec)"
    }
}

fn seatbelt_string_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str(r"\\"),
            '"' => escaped.push_str(r#"\""#),
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            c if c.is_control() => escaped.push('?'),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Generate a Seatbelt `.sb` policy with restrictive defaults.
///
/// The policy:
/// - Denies all network operations by default
/// - Allows DNS lookups and outbound connections to localhost only
/// - Denies filesystem writes outside the workspace and temp directories
/// - Allows reads to system paths required for process execution
/// - Restricts process spawning to essential operations
fn generate_policy(workspace: &Path, outbound_allow: &[String]) -> String {
    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    let workspace_str = seatbelt_string_literal(&workspace.to_string_lossy());
    let workspace_canonical_str = seatbelt_string_literal(&workspace_canonical.to_string_lossy());

    // Generate parent rules to support getcwd traversal
    let mut parent_rules_list = String::new();
    let mut seen_parents = std::collections::HashSet::new();

    let mut current = workspace.parent();
    while let Some(p) = current {
        let p_str = seatbelt_string_literal(&p.to_string_lossy());
        if !p_str.is_empty() && p_str != "/" && seen_parents.insert(p_str.clone()) {
            parent_rules_list.push_str(&format!("    (literal \"{}\")\n", p_str));
            let with_slash = if p_str.ends_with('/') {
                p_str.clone()
            } else {
                format!("{}/", p_str)
            };
            parent_rules_list.push_str(&format!("    (literal \"{}\")\n", with_slash));

            // Allow checking existence of common project files in parent dirs to prevent Bun/Node resolver EPERM crashes
            for file in &[
                "package.json",
                "node_modules",
                "tsconfig.json",
                "jsconfig.json",
                "bunfig.toml",
                ".env",
                "package-lock.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "bun.lockb",
            ] {
                parent_rules_list.push_str(&format!("    (literal \"{}{}\")\n", with_slash, file));
            }
        }
        current = p.parent();
    }

    let mut current_canonical = workspace_canonical.parent();
    while let Some(p) = current_canonical {
        let p_str = seatbelt_string_literal(&p.to_string_lossy());
        if !p_str.is_empty() && p_str != "/" && seen_parents.insert(p_str.clone()) {
            parent_rules_list.push_str(&format!("    (literal \"{}\")\n", p_str));
            let with_slash = if p_str.ends_with('/') {
                p_str.clone()
            } else {
                format!("{}/", p_str)
            };
            parent_rules_list.push_str(&format!("    (literal \"{}\")\n", with_slash));

            // Allow checking existence of common project files in parent dirs to prevent Bun/Node resolver EPERM crashes
            for file in &[
                "package.json",
                "node_modules",
                "tsconfig.json",
                "jsconfig.json",
                "bunfig.toml",
                ".env",
                "package-lock.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "bun.lockb",
            ] {
                parent_rules_list.push_str(&format!("    (literal \"{}{}\")\n", with_slash, file));
            }
        }
        current_canonical = p.parent();
    }

    let parent_rules = if parent_rules_list.is_empty() {
        String::new()
    } else {
        format!(
            "\n;; Allow reading parent directories of workspace (needed for getcwd traversal)\n(allow file-read*\n{})",
            parent_rules_list
        )
    };

    let workspace_read_rules = if workspace == workspace_canonical {
        format!("(allow file-read* (subpath \"{}\"))", workspace_str)
    } else {
        format!(
            "(allow file-read* (subpath \"{}\"))\n(allow file-read* (subpath \"{}\"))",
            workspace_str, workspace_canonical_str
        )
    };

    let workspace_write_rules = if workspace == workspace_canonical {
        format!("(allow file-write* (subpath \"{}\"))", workspace_str)
    } else {
        format!(
            "(allow file-write* (subpath \"{}\"))\n(allow file-write* (subpath \"{}\"))",
            workspace_str, workspace_canonical_str
        )
    };

    // Detect if HOME environment variable is overridden to a session-specific isolated directory
    let host_home = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf());
    let mut extra_home_rules = String::new();
    if let Ok(env_home_str) = std::env::var("HOME") {
        let env_home_path = PathBuf::from(&env_home_str);
        let is_isolated = if let Some(ref hh) = host_home {
            env_home_path != *hh
        } else {
            true
        };
        if is_isolated {
            let env_home_escaped = seatbelt_string_literal(&env_home_path.to_string_lossy());
            extra_home_rules.push_str(&format!(
                "\n;; Allow reading and writing to isolated session HOME\n(allow file-read* (subpath \"{}\"))\n(allow file-write* (subpath \"{}\"))\n",
                env_home_escaped, env_home_escaped
            ));
        }
    }

    // Render trusted outbound hosts. Each entry is rendered as its own
    // (allow network-outbound (remote tcp "host:port")) rule. Empty
    // list yields an empty string so the policy is unchanged.
    //
    // macOS sandbox-exec rejects `(remote tcp "host:port")` at policy
    // load time (it only accepts `localhost` or `*` in network filters),
    // so on macOS this block is intentionally a no-op — emitting the
    // rules would cause every shell spawn to fail with
    // `sandbox-exec: host must be * or localhost`. Operators who need
    // external access on macOS must use a localhost proxy or accept
    // the coarse `*` fallback; the config field is preserved for
    // forward compatibility with Linux/Windows backends.
    #[cfg(not(target_os = "macos"))]
    let outbound_allow_rules = if outbound_allow.is_empty() {
        String::new()
    } else {
        let mut s = String::from(
            "\n;; User-configured trusted outbound hosts (security.sandbox.network_outbound_allow)\n",
        );
        for entry in outbound_allow {
            let escaped = seatbelt_string_literal(entry);
            s.push_str(&format!(
                "(allow network-outbound\n    (remote tcp \"{}\"))\n",
                escaped
            ));
        }
        s
    };
    #[cfg(target_os = "macos")]
    let outbound_allow_rules = if outbound_allow.is_empty() {
        String::new()
    } else {
        let mut s = String::from(
            "\n;; User-configured trusted outbound hosts (mapped to *:port on macOS Seatbelt)\n",
        );
        for entry in outbound_allow {
            // macOS sandbox-exec rejects specific hostnames (e.g. "aiordering.kfc.com.cn:443"),
            // so we map "host:port" to a wildcard "*:port" rule.
            let port = entry.split(':').next_back().unwrap_or("*");
            s.push_str(&format!(
                "(allow network-outbound\n    (remote ip \"*:{}\"))\n",
                port
            ));
        }
        s
    };

    format!(
        r#"(version 1)

;; Deny everything by default
(deny default)

;; ── Process execution ──────────────────────────────────────
;; Allow basic process operations needed for command execution
(allow process-exec)
(allow process-fork)
(allow signal (target self))

;; ── Filesystem reads ───────────────────────────────────────
;; Allow reading system libraries, frameworks, and executables
(allow file-read*
    (subpath "/usr")
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/Library")
    (subpath "/System")
    (subpath "/private/var")
    (subpath "/private/etc")
    (subpath "/dev")
    (subpath "/etc")
    (subpath "/Applications")
    (subpath "/opt")
    (subpath "/nix")
    (literal "/")
    (subpath "/var"))

;; Allow reading the workspace
{workspace_read_rules}
{parent_rules}
{extra_home_rules}

;; Allow reading temp directories (needed for policy file itself)
(allow file-read* (subpath "/tmp"))
(allow file-read* (subpath "/private/tmp"))
(allow file-read*
    (regex #"^/private/var/folders/"))

;; Allow reading user home for tool configs
(allow file-read*
    (regex #"^/Users/[^/]+/\\."))

;; Allow traversing /Users for path resolution (needed for realpath resolution)
(allow file-read-metadata
    (subpath "/Users"))

;; ── Filesystem writes ──────────────────────────────────────
;; Only allow writes to workspace and temp directories
{workspace_write_rules}
(allow file-write*
    (subpath "/tmp")
    (subpath "/private/tmp"))
(allow file-write*
    (regex #"^/private/var/folders/"))
(allow file-write* (subpath "/dev/null"))
(allow file-write* (subpath "/dev/tty"))

;; ── Network ────────────────────────────────────────────────
;; Deny all network by default (inherited from deny default)
;; Allow DNS resolution only
(allow network-outbound
    (remote unix-socket (path-literal "/var/run/mDNSResponder"))
    (remote unix-socket (path-literal "/private/var/run/mDNSResponder")))
(allow system-socket)

;; Allow localhost connections only (for local dev servers).
;; Note: macOS sandbox-exec only accepts "localhost:*" or "*:port" in
;; (remote ip ...) filters — raw IP addresses cause the entire policy
;; to fail to parse.
(allow network-outbound
    (remote ip "localhost:*"))
{outbound_allow_rules}

;; ── Mach / IPC ─────────────────────────────────────────────
;; Allow basic mach services needed for process execution
(allow mach-lookup
    (global-name "com.apple.system.logger")
    (global-name "com.apple.system.notification_center")
    (global-name "com.apple.SecurityServer")
    (global-name "com.apple.CoreServices.coreservicesd"))

;; ── Sysctl / misc ──────────────────────────────────────────
(allow sysctl-read)
(allow mach-task-name)
"#,
        workspace_read_rules = workspace_read_rules,
        parent_rules = parent_rules,
        workspace_write_rules = workspace_write_rules,
        extra_home_rules = extra_home_rules,
        outbound_allow_rules = outbound_allow_rules,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_policy_is_valid_sb_format() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[]);
        assert!(policy.starts_with("(version 1)"));
        let open = policy.chars().filter(|c| *c == '(').count();
        let close = policy.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close, "parentheses must be balanced in .sb policy");
    }

    /// Regression: every top-level rule in the generated policy must be
    /// wrapped in `(allow ...)`. A missing `(allow ` prefix is a silent
    /// regression (parentheses still balance) that `sandbox-exec` rejects
    /// with `illegal function`. This test would have caught the
    /// 2026-06-10 incident where `(signal (target self))` shipped without
    /// its `(allow ` prefix and broke every shell invocation.
    ///
    /// We use `sandbox-exec` itself to validate the policy: writing a
    /// tmp policy file and asking sandbox-exec to load it via `-n quick`.
    /// This catches both the (allow prefix issue and any other
    /// syntax errors that pure parenthesis-balancing would miss.
    #[cfg(target_os = "macos")]
    #[test]
    fn generate_policy_loads_with_sandbox_exec() {
        let workspace = PathBuf::from("/tmp/workspace");
        // We use "*:443" (not a host:port) because macOS sandbox-exec
        // rejects `(remote tcp "host:port")` at policy-load time — it
        // only accepts `(remote ip "localhost:port")` or
        // `(remote ip "*:port")`. This test is about the surrounding
        // policy structure, not the network filter syntax.
        let policy = generate_policy(&workspace, &["*:443".to_string()]);

        // Write the policy to a temp file and ask sandbox-exec to load it.
        let tmp =
            std::env::temp_dir().join(format!("zeroclaw_seatbelt_test_{}.sb", std::process::id()));
        std::fs::write(&tmp, &policy).expect("write tmp policy");

        // `sandbox-exec -f policy true` exits 0 if the policy loads
        // without syntax errors. It also runs `true` under the policy
        // (which allows everything we need to spawn a no-op), so this
        // validates both the policy grammar and the rule semantics.
        let status = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&tmp)
            .arg("/bin/sh")
            .arg("-c")
            .arg("true")
            .status()
            .expect("spawn sandbox-exec");

        let _ = std::fs::remove_file(&tmp);

        assert!(
            status.success(),
            "sandbox-exec rejected the generated policy (status={:?}); this \
             usually means a missing `(allow ` prefix or other syntax error. \
             policy:\n{}",
            status.code(),
            policy,
        );
    }

    #[test]
    fn generate_policy_with_no_outbound_allow_has_no_trusted_host_rules() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &[]);
        assert!(
            !policy.contains("network_outbound_allow"),
            "empty allow list must not inject any trusted-host rule"
        );
        assert!(
            !policy.contains("(remote tcp"),
            "empty allow list must not introduce (remote tcp ...) rules"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn generate_policy_with_outbound_allow_renders_wildcard_ip_rules_on_macos() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(
            &workspace,
            &[
                "aiordering.kfc.com.cn:443".to_string(),
                "api.example.com:8443".to_string(),
            ],
        );
        assert!(
            policy.contains("(allow network-outbound\n    (remote ip \"*:443\"))"),
            "policy must map KFC API host to wildcard port 443 rule, got: {}",
            policy
        );
        assert!(
            policy.contains("(allow network-outbound\n    (remote ip \"*:8443\"))"),
            "policy must map second host to wildcard port 8443 rule"
        );
        // Sanity: parentheses still balanced.
        let open = policy.chars().filter(|c| *c == '(').count();
        let close = policy.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close, "parentheses must be balanced in .sb policy");
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires live internet connection and dns resolution"]
    fn test_sandbox_network_access() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace, &["google.com:443".to_string()]);

        let tmp = std::env::temp_dir().join(format!(
            "zeroclaw_seatbelt_test_net_{}.sb",
            std::process::id()
        ));
        std::fs::write(&tmp, &policy).expect("write tmp policy");

        let status = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&tmp)
            .arg("curl")
            .arg("-I")
            .arg("-s")
            .arg("--connect-timeout")
            .arg("5")
            .arg("https://www.google.com")
            .status();

        let _ = std::fs::remove_file(&tmp);

        match status {
            Ok(s) => {
                assert!(s.success(), "sandbox curl failed with exit status: {:?}", s);
            }
            Err(e) => {
                panic!("failed to spawn sandbox-exec curl: {:?}", e);
            }
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn generate_policy_with_outbound_allow_renders_trusted_host_rules() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(
            &workspace,
            &[
                "aiordering.kfc.com.cn:443".to_string(),
                "api.example.com:8443".to_string(),
            ],
        );
        // Each entry is rendered as its own (allow network-outbound (remote tcp "host:port")) rule.
        assert!(
            policy.contains(
                "(allow network-outbound\n    (remote tcp \"aiordering.kfc.com.cn:443\"))"
            ),
            "policy must include the KFC API outbound rule, got: {}",
            policy
        );
        assert!(
            policy.contains("(allow network-outbound\n    (remote tcp \"api.example.com:8443\"))"),
            "policy must include the second outbound rule"
        );
        // Section comment is rendered so operators can see where the rules came from.
        assert!(
            policy.contains(";; User-configured trusted outbound hosts"),
            "policy must include the trusted-hosts section comment"
        );
        // Sanity: parentheses still balanced.
        let open = policy.chars().filter(|c| *c == '(').count();
        let close = policy.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close, "parentheses must be balanced in .sb policy");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn generate_policy_escapes_quotes_in_outbound_allow_entries() {
        // A malformed config entry with an embedded quote must not break out of
        // the policy string literal.
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(
            &workspace,
            &["evil\"; (allow process-exec); \"".to_string()],
        );
        // The escaped form replaces " with \"
        assert!(
            policy.contains("evil\\\"; (allow process-exec); \\\""),
            "embedded quotes must be backslash-escaped, got: {}",
            policy
        );
        // And the policy itself must still parse (balanced parens).
        let open = policy.chars().filter(|c| *c == '(').count();
        let close = policy.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close);
    }
}
