# macOS Seatbelt Sandbox getcwd Traversal and isolated HOME Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve macOS Seatbelt sandbox `sandbox-exec` EPERM/getcwd errors by canonicalizing the workspace path, allowing trailing slashes for parent directories, and granting read/write access to the isolated session `HOME` directory if it is overridden.

**Architecture:** 
1. Update `seatbelt.rs` to detect if the `HOME` environment variable is overridden to a session-specific isolated directory (different from the host's actual home directory).
2. If `HOME` is isolated, grant full read/write (`file-read*` and `file-write*`) access to the isolated `HOME` directory to allow tool configurations (like `.kfc-skill`) to be created and read.
3. Canonicalize the workspace path to handle resolved vs unresolved paths and add trailing slash literal rules for all parent directories to support POSIX directory traversal.

**Tech Stack:** Rust 2024, Apple Seatbelt Sandboxing (macOS `sandbox-exec`), `directories` crate.

---

### Task 1: Add Unit Tests for Trailing Slashes and Isolated HOME

**Files:**
- Modify: `crates/zeroclaw-runtime/src/security/seatbelt.rs:266-490`

- [ ] **Step 1: Write the tests**

Add the following tests to the `tests` module in `crates/zeroclaw-runtime/src/security/seatbelt.rs`:

```rust
    #[test]
    fn generate_policy_allows_parent_directories_with_trailing_slash() {
        let workspace = PathBuf::from("/tmp/workspace-test");
        let policy = generate_policy(&workspace);
        // Verify parent directories both with and without trailing slash are allowed
        assert!(policy.contains(r#"(literal "/tmp")"#));
        assert!(policy.contains(r#"(literal "/tmp/")"#));
    }

    #[test]
    fn generate_policy_includes_canonicalized_workspace_paths() {
        // Use /tmp which resolves to /private/tmp on macOS
        let workspace = PathBuf::from("/tmp/workspace-test");
        let policy = generate_policy(&workspace);
        assert!(policy.contains(r#"(allow file-read* (subpath "/tmp/workspace-test"))"#));
        assert!(policy.contains(r#"(allow file-read* (subpath "/private/tmp/workspace-test"))"#));
    }
```

- [ ] **Step 2: Run tests to verify they fail/pass**

Run: `cargo test --package zeroclaw-runtime --lib security::seatbelt::tests`
Expected: Passes (with trailing slash and canonicalization code already partially in place, but we need to implement the isolated HOME fix).

---

### Task 2: Implement Isolated HOME and Parents Fix in seatbelt.rs

**Files:**
- Modify: `crates/zeroclaw-runtime/src/security/seatbelt.rs:155-265`

- [ ] **Step 1: Update generate_policy implementation**

Replace the `generate_policy` function in `crates/zeroclaw-runtime/src/security/seatbelt.rs` to support isolated HOME directory access:

```rust
fn generate_policy(workspace: &Path) -> String {
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
    (remote unix-socket (path-literal "/var/run/mDNSResponder")))
(allow system-socket)

;; Allow localhost connections only (for local dev servers).
;; Note: macOS sandbox-exec only accepts "localhost:*" or "*:port" in
;; (remote ip ...) filters — raw IP addresses cause the entire policy
;; to fail to parse.
(allow network-outbound
    (remote ip "localhost:*"))

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
    )
}
```

- [ ] **Step 2: Run cargo tests to verify compilation and passing status**

Run: `cargo test --package zeroclaw-runtime --lib security::seatbelt::tests`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/zeroclaw-runtime/src/security/seatbelt.rs
git commit -m "feat: grant read/write access to isolated session HOME in seatbelt sandbox"
```
