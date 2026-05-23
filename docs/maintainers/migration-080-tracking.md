# Migration Tracking — master → 0.8.0

> Snapshot date: 2026-05-21 (updated as ports land)
> Base: `0.8.0` branch (= upstream `c746998f6` + local ports)
> Source: 215 commits on local `master` ahead of `upstream/master`
>   - 140 non-merge commits
>   - 75 merge commits (PR landings + upstream syncs)

This report tracks which local-fork functionality has been ported to the
0.8.0 architecture and what remains. Status legend:

- ✅ **Migrated** — feature fully ported to 0.8.0
- 🟡 **Partial** — feature partially ported (caveats noted)
- ⏸️ **Deferred** — deliberately not migrated; superseded by upstream or scheduled for later
- ❌ **Pending** — not yet evaluated or scheduled
- 🟪 **Glue** — merge commits or chore commits with no functional payload

## Headline Summary

| Area | Commits | Status | Owner of completion |
|------|---------|--------|---------------------|
| **A. WuKongIM channel** | ~70 | ✅ Migrated (minus progress_streaming) | `7759e1d52` |
| **B. dawn_s3 / DawnS3Tool** | 9 | ✅ Migrated (new `dawn-tools` crate) | `688cd30a7` |
| **C. Web search routing (YumcSearch)** | 2 | ✅ Migrated | `d608b8f1b` |
| **D. Local logging refactor** | 9 | ⏸️ Deferred (superseded by `zeroclaw-log`) | — |
| **E. progress-observer crate** | 13 | ⏸️ Deferred (superseded by `zeroclaw-log`/Observer bridge) | — |
| **F. Windows/PowerShell hardening** | 5 | ✅ Migrated (squashed) | `7eaed77e4` |
| **G. Skills `enabled` field** | 6 | ✅ Migrated (squashed; upstream had no equivalent) | `f6199e8bb` |
| **H. Provider routing extensions** | 1 | 🟡 Partial (temperature ported; max_tokens superseded by upstream alias mechanism) | `7e3c5da73` |
| **I. Channel/orchestrator misc** | 8 | ❌ Pending | — |
| **J. Multimodal / Lark image fixes** | 11 | ❌ Pending (may be redundant with upstream) | — |
| **K. UTF-8 / truncation fixes** | 2 | ✅ Migrated (1 ported, 1 moot — upstream removed code path) | `225d3a670` |
| **L. System-prompt additions (S3, Node.js)** | 3 | ✅ Migrated (3 commits squashed; LLM-logging half of `aff780a82` skipped) | `b46b802de` |
| **M. Docs / housekeeping** | 5 | ❌ Pending (low priority) | — |
| **N. Merge / chore glue** | ~75 | 🟪 N/A | — |

---

## A. WuKongIM channel (✅ Migrated — 1 commit on 0.8.0)

All listed functionality was unified into a single port commit on 0.8.0:
**`7759e1d52` feat(channels/wukongim): port WuKongIM channel to 0.8.0 architecture**

The port covers everything in this list **except** the explicitly-deferred
items at the bottom.

### A.1 Scaffolding (separate crate → inlined sub-module)

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `67c52ee63` | chore(channels): scaffold zeroclaw-channel-wukongim crate with 5 domain modules |
| ✅ | `d24a550ff` | chore: add default-features = false to zeroclaw-channel-wukongim |
| ✅ | `c6f54c1f0` | feat(channel-wukongim): add connection module — JSON-RPC types + WS constants |
| ✅ | `ec4813f65` | feat(channel-wukongim): add messaging module — media download + payload encoding |
| ✅ | `a74d44b12` | feat(channel-wukongim): add filter module — allowlist + mention detection |
| ✅ | `047092a90` | feat(channel-wukongim): add approval module — card types + pending state alias |
| ✅ | `36425401b` | feat(channel-wukongim): add config module — re-export WuKongIMConfig |
| ✅ | `e1371de5c` | feat(channel-wukongim): implement WuKongIMChannel orchestrating all 5 modules |
| ✅ | `ab93c51a0` | fix(channel-wukongim): send_rpc leak, health_check URL, FILE constant, timestamp cast |
| ✅ | `7f00d5230` | refactor(channels): wire zeroclaw-channel-wukongim as optional dep |
| ✅ | `4f9099eb0` | fix(channels): import WuKongIMChannel via crate re-export |
| ✅ | `83b852aad` | style(channel-wukongim): cargo fmt |

> **0.8.0 change**: inlined as `crates/zeroclaw-channels/src/wukongim/` sub-module (not separate crate).

### A.2 JSON-RPC integration + initial message handling

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `365f0b616` | feat(wukongim): 集成 WuKongIM JSON-RPC 消息频道 |
| ✅ | `4cf83c1b0` | feat(wukongim): fix message delivery and add group chat support |
| ✅ | `cb6e31424` | feat(channel): 支持WuKongIM的Markdown消息类型处理 |
| ✅ | `c27e8e6ec` | feat(channel): 增强WuKongIM channel的文件上传功能 |
| ✅ | `7a7c7cfb8` | fix(wukongim): 修复编译错误和 clippy 警告 |
| ✅ | `12d8b1c7c` | style: 格式化代码并修复 lint 问题 |
| ✅ | `e0292befa` | feat(channel-wukongim): read device_id and device_flag from config |
| ✅ | `cb1d62a72` | feat(channels/wukongim): read device_id and device_flag from config (dup branch) |

### A.3 Approval flow (v1 → v2 → polish)

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `c27d44db1` | feat(wukongim): define structs for approval flow v2 |
| ✅ | `901306c97` | refactor(wukongim): use WkApprovalCard struct for outbound cards |
| ✅ | `c9629fbcb` | feat(wukongim): implement strict type 21 check for approval responses |
| ✅ | `903e1ed1e` | test(wukongim): add tests for approval flow structs |
| ✅ | `13d12dbb7` | feat(wukongim): simplify approval UI and refactor message types to constants |
| ✅ | `fe05425ad` | feat(wukongim): (dup) simplify approval UI and refactor message types |
| ✅ | `1c04e9811` | feat: 优化 WuKongIM 审批流卡片及消息发送接收逻辑 |
| ✅ | `5be6a0e7c` | fix(channels): add missing actions field in WkApprovalCard test |
| ✅ | `3939c90b9` | feat(channels): localize approval card title to Chinese |
| ✅ | `34d9a05a7` | merge: feat/wukongim-approval-v2 |

### A.4 Mention detection + group chat

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `2ee9d901f` | feat(wukongim): implement mention-only response logic for group chats |
| ✅ | `784a119ff` | feat(wukongim): improve mention detection and add logging |
| ✅ | `579db5b47` | 过滤掉自己 (drop self-loop messages) |
| ✅ | `6f4814bcf` | chore: collapse nested if for 'all' mention check |
| ✅ | `dd1b46a35` | chore: collapse nested if for 'uids' mention check |
| ✅ | `13c7b0490` | chore: collapse nested if for content mention check |
| ✅ | `d2fdf4a09` | feat(wukongim): fix @all mention logic and markdown content extraction |
| ✅ | `f55b82a00` | 修改群聊识别上下文 |
| ✅ | `ab343ed21` | feat(channel): implement background memory for non-mentioned wukongim group messages |
| ✅ | `6d3524854` | fix(wukongim): move ACK after mention_only check to prevent message loss |

### A.5 File download / media handling

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `d905d9396` | feat(wukongim): add blocked extension checking utility |
| ✅ | `d5b7b52db` | feat(wukongim): add markdown link extraction utility |
| ✅ | `7d33de305` | feat(wukongim): add file download to workspace function |
| ✅ | `c294711e4` | feat(wukongim): rename and extend process_markdown_resources to handle files |
| ✅ | `ba54567c6` | feat(wukongim): update re-exports for file download functionality |
| ✅ | `b0203af0b` | feat(wukongim): add workspace_dir field to WuKongIMChannel |
| ✅ | `d69692c72` | feat(wukongim): use process_markdown_resources in message handling |
| ✅ | `a488a45ff` | feat(wukongim): pass workspace_dir to WuKongIMChannel |
| ✅ | `696a668ef` | fix(wukongim): address clippy warning and formatting |
| ✅ | `71edf265b` | feat(wukongim): download type=5 FILE messages to workspace |
| ✅ | `647fbec8b` | docs: mark WuKongIM file download feature as implemented |
| ✅ | `43f804efd` | feat(channel-wukongim): support configurable downloads_dir |
| ✅ | `061800c66` | style: fix fmt issues |
| ✅ | `443b900bd` | fix(wukongim): remove unused workspace_dir field |
| ✅ | `4f62a5dda` | remote record.dir to allowed_roots |

### A.6 History sync / offline messages

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `a60b478d5` | feat: WuKongIM historical message sync via memory traits |
| ✅ | `b41f7ea17` | fix(wukongim): align sync protocol + file-based state persistence |
| ✅ | `66fcca3f4` | 提交离线消息处理 (offline message handling) |
| ✅ | `590582ac4` | modify offline |
| ✅ | `e3330032a` | feat(channels/wukongim): handle la_init_helloworld CMD with mention check |

### A.7 Misc fixes

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `9da1fbee5` | fix(wukongim): spawn inbound processing so listen loop never self-deadlocks |
| ✅ | `912b40610` | fix payload_b64 question |
| ✅ | `b4963ca49` | reactions add |
| ✅ | `78b3ae0bb` | merge from master |
| ✅ | `fbea7dc70` | feat(channels): improve wukongim channel support and cron scheduling (cron part deferred — see I) |

### A.8 ⏸️ DEFERRED: progress_streaming / send_status_update

These commits implemented the realtime status push (type=23 cards).
Per user decision, **not migrated in this pass** — `progress_streaming`
config field is kept as a no-op until upstream's Observer-bridge story
lands and we can re-implement on top of it.

| Status | Commit | Title |
|--------|--------|-------|
| ⏸️ | `54e88a764` | feat(channel-wukongim): add progress_streaming opt-in field |
| ⏸️ | `645de638a` | feat(channel-wukongim): implement send_status_update and send_cmd_message |
| ⏸️ | `fb9d11b4d` | fix(wukongim): base64-encode cmd payload before sending |
| ⏸️ | `38d0e716a` | fix(wukongim): change status update to type=23 content message |
| ⏸️ | `7bb9ecd4a` | refactor(wukongim): rename send_cmd_message to send_status_message |
| ⏸️ | `af1aef21e` | feat(wukongim): update default ack message |
| ⏸️ | `f2fbd542e` | fix(channel-wukongim): add failure messages and from_config mapping |

---

## B. dawn_s3 / DawnS3Tool (✅ Migrated — 1 commit on 0.8.0)

Ported into a brand-new workspace crate `crates/dawn-tools/` instead of
adding to `zeroclaw-tools/`, so Dawn SaaS integrations stay isolated and
can grow (history sync, etc.) without bloating the main tools crate.

Owner of completion: **`688cd30a7` feat(dawn-tools): add dawn-tools crate with DawnS3Tool**

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `9d0b74f5d` | feat(tools): add DawnS3Tool for file upload to S3 |
| ✅ | `75a9cd783` | feat(channels): add Dawn S3 tool description to orchestrator system prompt |
| ✅ | `80cded0c4` | feat(dawn_s3): add Dawn S3 file upload tool with security hardening |
| ✅ | `db219dc6c` | fix(dawn_s3): add security path validation for file uploads |
| ✅ | `d2f9f27f5` | fix(dawn_s3): fix guess_content_type argument type |
| ✅ | `dd421397a` | test(dawn_s3): add execute() path tests for error branches |
| ✅ | `53aedfb05` | fix(dawn_s3): avoid leaking raw user paths in LLM-facing error messages |
| 🟡 | `aff780a82` | feat(runtime): add S3 file sharing prompt and enhanced LLM logging — tool description placed in `tool_descs`; broader "File Sharing" prompt section deferred (master had it in `system_prompt.rs` but 0.8.0's per-tool description in `tool_descs` is canonical) |
| 🟡 | `f58ae18c6` | feat: implement centralized configuration schema and web search tool integration — DawnS3 config schema portion migrated; web search piece tracked in area C |

> **0.8.0 changes**: new `dawn-tools` Cargo feature gates the optional
> dependency at compile time; runtime `[dawn_s3]` config still gates
> registration at startup. New `ToolKind::DawnS3` variant added for
> attribution. `$DAWN_S3_TOKEN` env-var fallback preserved.

---

## C. Web search routing — YumcSearch (✅ Migrated — 1 commit on 0.8.0)

Ported via **`d608b8f1b` feat(web_search): add YumcSearch provider route**.

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `3480e0857` | feat: implement extensible web search tool with lazy configuration |
| ✅ | (shared with `f58ae18c6` schema portion) | YumcSearch alias resolution in `web_search_provider_routing.rs` |

> **0.8.0 changes**: new `WebSearchProviderRoute::YumcSearch` variant,
> 2 new `WebSearchConfig` fields (`yumc_search_api_key` secret +
> `yumc_search_base_url`), 2 new args on `WebSearchTool::new_with_config`,
> 4 new methods (`resolve_yumc_search_api_key`,
> `reload_yumc_search_api_key`, `search_yumc_search`,
> `parse_yumc_search_results`). All log emission via `zeroclaw-log`.
> Lazy key reload + decrypt matches the Brave/Tavily pattern.

---

## D. Local logging refactor (⏸️ Deferred)

**Decision**: superseded by upstream's `zeroclaw-log` crate. All future
log emission goes through `::zeroclaw_log::record!`. These local commits
will not be ported.

| Status | Commit | Title |
|--------|--------|-------|
| ⏸️ | `c96131ff3` | feat(logging): add [logging] config section for log level and file output |
| ⏸️ | `8ea6e9c55` | refactor(logging): flatten config to dir/out_file/err_file fields |
| ⏸️ | `412ec68dd` | fix(logging): read --config-dir arg to find config.toml for early loading |
| ⏸️ | `26603ee69` | fix(logging): reliable toml deserialization and visible error diagnostics |
| ⏸️ | `be8f6fdbd` | debug(logging): write diagnostic file to config_dir on startup |
| ⏸️ | `999e9582f` | refactor(logging): replace file-based diagnostics with eprintln |
| ⏸️ | `6a2cfad24` | fix(logging): replace remaining write_init_error call with eprintln |
| ⏸️ | `a9f7523ab` | fix(logging): use EarlyConfig wrapper instead of toml::Value parsing |

**Follow-up**: verify upstream `zeroclaw-log` has equivalent file-output
configuration. If a missing knob comes up later (e.g. dir/out_file/err_file
split), file a focused PR upstream instead of re-porting.

---

## E. progress-observer crate (⏸️ Deferred)

**Decision**: superseded by upstream's `zeroclaw-log` event broadcast +
Observer bridge in 0.8.0. The whole `crates/zeroclaw-progress-observer/`
will not be ported.

| Status | Commit | Title |
|--------|--------|-------|
| ⏸️ | `c4de33b77` | feat(progress-observer): scaffold new crate |
| ⏸️ | `3db39131f` | feat(progress-observer): add toggles, summarize_tool_args, event_to_status |
| ⏸️ | `94bdb3db4` | test(progress-observer): add MockChannel test helper |
| ⏸️ | `d335ec535` | feat(progress-observer): implement ProgressReportingObserver |
| ⏸️ | `1c91e91a` (`1c1e90c91`) | feat(channels): wire progress-reporting observer into orchestrator |
| ⏸️ | `d088b2202` | feat(channels): wire ProgressReportingObserver into orchestrator |
| ⏸️ | `019f180ff` | feat(orchestrator): fire AgentStart/AgentEnd events for channel turns |
| ⏸️ | `cdbf37e53` | fix(progress-observer): fix tool names in desc mapping |
| ⏸️ | `8445bba23` | fix(progress-observer): friendly tool label in tool_call done/fail desc |
| ⏸️ | `73c20ebad` | feat(progress-observer): event_name helper for readable log output |
| ⏸️ | `ac823f870` | feat(api): add StatusUpdate and Channel::send_status_update default |
| ⏸️ | `6e759624d` | feat(config): add [progress_observer] config section |
| ⏸️ | `d0b2055a4` | fix(config): correct derive order and fix async test for ProgressObserverConfig |
| ⏸️ | `e5245a891` | chore: update Cargo.lock for zeroclaw-progress-observer crate |
| ⏸️ | `d957d2ee5` | docs(plan): add progress streaming implementation plan |
| ⏸️ | `76bf5b723` | docs(spec): add progress streaming via sidelined observer design |

**Follow-up**: when WuKongIM `progress_streaming` is ready to be re-enabled,
do it via Observer bridge subscription inside the WK module — not by
reviving this crate.

---

## F. Windows / PowerShell hardening (✅ Migrated — 1 commit on 0.8.0)

All 5 master commits squashed into **`7eaed77e4` feat(shell,security): Windows/PowerShell hardening**.

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `619ea3eb1` | feat(shell): use PowerShell instead of cmd.exe on Windows |
| ✅ | `d168b641a` | feat(shell): prefer pwsh.exe, fall back to powershell.exe then cmd.exe |
| ✅ | `7ac36e373` | fix(security): peel powershell/cmd wrappers before allowlist check |
| ✅ | `f1b1a174b` | fix(security): case-insensitive allowed_commands matching on Windows |
| ✅ | `fb9df1151` | fix(security): clarify allowed_roots in prompt and tool description |

> Already in upstream: `c746998f6 fix(policy): allow multiline heredocs in SecurityPolicy command splitting (#6816)`

> **0.8.0 changes**: pwsh/powershell/cmd detection cached on
> `NativeRuntime` via `WindowsShell` enum; `shell-words` dep added to
> `zeroclaw-config` for the wrapper peeler; 4 new policy unit tests
> covering case-insensitive matching, wrapper unwrapping, and
> EncodedCommand rejection; 2 new NativeRuntime tests for shell
> selection. Skipped the `[logging]` Default-impl additions from
> `f1b1a174b` (those belong to deferred Area D).

---

## G. Skills `enabled` field (✅ Migrated — 1 commit on 0.8.0)

Confirmed: upstream 0.8.0's skills refactor (new submodules bundle.rs,
constants.rs, document.rs, frontmatter.rs, reference.rs, scaffold.rs,
service.rs) did NOT add an enabled/disabled field — port was needed.

All 6 master commits squashed into **`f6199e8bb` feat(skills): support per-skill enabled flag**.

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `52a93c369` | test(skills): add failing tests for enabled field (TDD) |
| ✅ | `a3b493ad1` | test(skills): clarify design contract in disabled_skill_excluded_from_prompt |
| ✅ | `76e1a3515` | feat(skills): add enabled field to Skill, SkillMeta, SkillMarkdownMeta |
| 🟡 | `ee7a78775` | feat(skills): wire enabled field through loaders and frontmatter parser — **skipped** the `doctor/mod.rs max_tokens/temperature` artifact that belongs to Area H |
| ✅ | `ba50948f9` | feat(skills): filter disabled skills from prompt and tool registry |
| ✅ | `ceb83c2bb` | feat(skills): show [disabled] badge in skills list |

> **0.8.0 changes**: serde `default_true` helper added next to existing
> `default_version`. 5 new unit tests (parse `enabled: true|false`,
> default to `None`, alias values `yes/on/1/no/off/0`, disabled skill
> excluded from system prompt). 4 test-only `Skill { ... }` literal
> sites filled in with `enabled: true,`. All 146 skills tests pass.

---

## H. Provider routing — per-route temperature (🟡 Partial — 1 commit on 0.8.0)

Master commit covered both `max_tokens` and `temperature`; ported only
the temperature half. Reason: upstream 0.8.0 already supports per-route
max-token budgets via per-alias config — declaring two aliases for the
same provider family with distinct `max_tokens` and pointing two routes
at them yields the same outcome as master's synthetic
`{provider}::max_tokens::{n}` mechanism, without the new code path.
Upstream temperature, on the other hand, is read once at agent setup
(`agent.rs:928-932`) and is not switchable per-route — that gap is
what this port closes.

Owner of completion: **`7e3c5da73` feat(providers): per-route temperature override on model_routes**.

| Status | Commit | Title |
|--------|--------|-------|
| 🟡 | `68109993d` | feat(providers): per-route max_tokens and temperature overrides in model_routes — temperature half ported; max_tokens half deliberately skipped |

> **Precedence**: when a route's `temperature` is set, it wins over the
> caller-supplied value (typically the agent's resolved
> `[model_providers.<alias>].temperature`). When unset, the caller
> value passes through. Bare model names (no `"hint:"` prefix) always
> pass through. 4 new router unit tests lock the contract.

> **For per-route max_tokens** on this branch, declare additional
> aliases under the same family, e.g.
> `[model_providers.openrouter.fast] max_tokens = 512` plus
> `[model_providers.openrouter.creative] max_tokens = 4096`, then point
> the two routes at `openrouter.fast` and `openrouter.creative`. The
> `options_for_provider_ref` machinery (`providers/lib.rs:779`) builds
> a distinct `ModelProviderRuntimeOptions` per alias, with its own
> `provider_max_tokens`, which the factory threads into each provider
> build.

---

## I. Channels / orchestrator misc (❌ Pending)

| Status | Commit | Title |
|--------|--------|-------|
| ❌ | `f10b6b0a8` | feat: Add working_dir parameter to shell tool and skill_directory to system prompt |
| ❌ | `37284dcc7` | feat(channels): structured error codes + context-window user message |
| ❌ | `fbea7dc70` | feat(channels): cron scheduling part (wukongim part already in A) |
| ❌ | `f96238e45` | fix(channels): teach reply-intent classifier to honor image attachments |
| ❌ | `80db3718d` | fix(channels,runtime): preserve tool_calls JSON across consecutive assistants |
| ❌ | `2a9e3c488` | docs(contributing): add zh-CN developer guide |
| ❌ | `f01000375` | chore: sync version references to v0.7.0 |
| ❌ | `bbbb54931` | docs: improve AGENTS.md with approval architecture and expanded skills |

---

## J. Multimodal / Lark image fixes (❌ Pending — may overlap upstream)

| Status | Commit | Title |
|--------|--------|-------|
| ❌ | `915472ebe` | fix(channels/lark): authorize approval responder + webhook limitation doc |
| ❌ | `d51e2eea1` | fix(channels/lark): migrate approval card to V2 schema (column_set + behaviors) |
| ❌ | `056dc187b` | fix(channels/lark): use message-resource endpoint + persist to session workspace |
| ❌ | `107c234aa` | chore(channels/lark): include URL and response body in image-download error log |
| ❌ | `74df326e7` | fix(channels/lark): flatten image save path + strip Windows verbatim prefix |
| ❌ | `17773efcb` | fix(multimodal,channels/lark): tolerate verbatim paths + stop [IMAGE:] for failed |
| ❌ | `380efec7e` | fix(multimodal): drop unresolvable image markers instead of aborting LLM call |
| ❌ | `622d674b9` | fix(providers/compatible): drop image refs that aren't data: or http(s) URLs |
| ❌ | `c2f79ee7d` | fix(channels,runtime): strip [IMAGE:] markers in text-only LLM helpers |
| ❌ | `f96238e45` | (also in I) reply-intent classifier honors image attachments |

**Recommendation**: diff each against upstream `lark.rs` / `multimodal.rs`
before porting — upstream may have done similar work independently.

---

## K. UTF-8 / truncation fixes (✅ Migrated — 1 commit on 0.8.0)

Ported via **`225d3a670` fix(tools/linkedin): prevent UTF-8 boundary panic when truncating**.

| Status | Commit | Title |
|--------|--------|-------|
| ✅ | `b0f06b470` | fix(tools): prevent UTF-8 boundary panic in LinkedIn string truncation — both `linkedin.rs:246` and `linkedin_client.rs:1188` slice sites fixed with `is_char_boundary` walk-back |
| ⏸️ | `6226cbc05` | fix(runtime): prevent panic when truncating multi-byte UTF-8 responses — **moot**: upstream 0.8.0 removed the buggy `&response_text[..1000]` debug-log path in `agent/loop_.rs` entirely, no port needed |

---

## L. System-prompt additions (✅ Migrated — 1 commit on 0.8.0)

3 master commits squashed into **`b46b802de` feat(prompt,skills,shell): port misc system-prompt + working_dir additions**.

| Status | Commit | Title |
|--------|--------|-------|
| 🟡 | `aff780a82` | feat(runtime): add S3 file sharing prompt and enhanced LLM logging — **prompt half ported**; LLM-logging half deliberately skipped (tracing-disallowed, carries the UTF-8 bug Area K fixed elsewhere, duplicates `zeroclaw_log` Action::Invoke/Receive at the provider boundary) |
| ✅ | `7c796a124` | feat: add Node.js Runtime Preference to system prompt (bun > node/npm) |
| ✅ | `f10b6b0a8` | feat: Add working_dir parameter to shell tool and skill_directory to system prompt — full port including security validation (working_dir must be inside workspace or `workspace/skills/`) |

> **0.8.0 changes**: `tool_descs` entry for `dawn_s3` (added in `688cd30a7`)
> plus the broader "File Sharing" prose section (added here) together give
> the LLM both the concise tool spec and the workflow guidance. The
> `<skill_directory>` element + `<usage>` block in skill prompts pair with
> the new `working_dir` shell parameter so third-party skills shipping
> `bash scripts/make.sh run`-style entries work without source edits.
> 3 new shell unit tests cover schema shape, rejection of out-of-workspace
> paths, and acceptance of `workspace/skills/...` paths.

---

## M. Docs / housekeeping (❌ Pending — low priority)

| Status | Commit | Title |
|--------|--------|-------|
| ❌ | `e95a51288` | docs: WuKongIM file download implementation plan (historical — can drop) |
| ❌ | `404888fa4` | docs: clarify blacklisted file handling in data flow |
| ❌ | `3cdc23601` | docs: WuKongIM file download feature design spec (historical — can drop) |
| ❌ | `d957d2ee5` | docs(plan): progress streaming implementation plan (paired with E — deferred) |
| ❌ | `76bf5b723` | docs(spec): progress streaming via sidelined observer (paired with E — deferred) |
| ❌ | `5e7d31196` | add (no message — investigate) |
| ❌ | `8bfe2d61c` | fmt modify (no functional payload — likely chore) |

---

## N. Merge / chore glue (🟪 N/A)

~75 commits are merge/chore glue: PR merges from contributor branches
(yumchina/denny, yumchina/mengliang, yumchina/cjj, etc.), upstream syncs,
and Cargo.lock updates. These have no functional payload to migrate and
are skipped.

Examples:
- `c9b352fa2` Merge pull request #31 from yumchina/feat/dawn-s3-integration
- `91f172f96` Merge pull request #30 from yumchina/mengliang
- `9f50ecfaf` Merge pull request #29 from yumchina/leohuai_feat
- `bdcf98783` Merge pull request #26 from yumchina/feat/dawn-s3-integration
- `ed52fbbb8` Merge remote-tracking branch 'upstream/master'
- ... etc.

---

## Recommended next migration order

Based on dependency and risk:

1. ~~**K. UTF-8 fixes**~~ — ✅ done (`225d3a670`)
2. ~~**F. Windows/PowerShell hardening**~~ — ✅ done (`7eaed77e4`)
3. ~~**C. YumcSearch route**~~ — ✅ done (`d608b8f1b`)
4. ~~**B. dawn_s3**~~ — ✅ done (`688cd30a7`)
5. ~~**G. Skills `enabled` field**~~ — ✅ done (`f6199e8bb`)
6. ~~**H. Per-route temperature**~~ — ✅ done (`7e3c5da73`); per-route max_tokens left to upstream alias mechanism
7. **I. Channels/orchestrator misc** — case-by-case
8. **J. Multimodal/Lark fixes** — diff against upstream first; many may be redundant
9. ~~**L. Node.js prompt addition + File Sharing + working_dir**~~ — ✅ done (`b46b802de`)

⏸️ **Deferred (no action):** D (local logging), E (progress-observer crate), parts of A (progress_streaming).

🟪 **Skipped:** N (glue commits).

## Verification

After each batch is migrated, run:
- `cargo check --all-targets --features channel-wukongim`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --workspace`
- `./dev/ci.sh all` for full validation

Update this report as commits move from ❌ → ✅.
