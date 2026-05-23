//! Main WuKongIM channel — struct, RPC plumbing, listen loop, Channel trait impl.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::schema::WuKongIMConfig;

use super::approval::{PendingApprovals, WkApprovalAction, build_approval_card};
use super::connection::{
    ClearUnreadRequest, ConnectParams, HEARTBEAT_TIMEOUT, Header, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, PING_INTERVAL, RecvAckParams, RecvNotificationParams,
    SendParams, SyncRequest, SyncResponse, WUKONGIM_RPC_VERSION, WkChannelType, WkMessageType,
    WsSink,
};
use super::filter::{is_mentioned, is_user_allowed, parse_recipient};
use super::messaging::{
    download_file_to_workspace, download_image_as_base64, encode_text_payload,
    process_markdown_resources,
};

#[derive(serde::Serialize, serde::Deserialize, Default, Debug)]
struct SyncState {
    max_version: i64,
    channel_seqs: HashMap<String, u32>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct WuKongIMChannel {
    pub(crate) alias: String,
    pub(crate) ws_url: String,
    pub(crate) uid: String,
    pub(crate) token: String,
    pub(crate) device_id: String,
    pub(crate) device_flag: i32,
    pub(crate) allowed_users: Vec<String>,
    pub(crate) approval_timeout_secs: u64,
    pub(crate) mention_only: bool,
    pub(crate) dawn_url: String,
    pub(crate) dawn_token: String,
    /// Reserved: throttled ack reply (`ack_reactions_message` after delay
    /// `ack_reactions_delay_secs`). The send path was commented out on
    /// master pending design clarification. Fields kept so the config
    /// surface stays stable.
    pub(crate) ack_reactions: bool,
    pub(crate) ack_reactions_message: String,
    pub(crate) ack_reactions_delay_secs: u64,
    pub(crate) memory: Arc<dyn Memory>,
    pub(crate) pending_responses:
        Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    pub(crate) pending_approvals: Arc<PendingApprovals>,
    pub(crate) ws_sink: Arc<RwLock<Option<WsSink>>>,
    pub(crate) downloads_dir: PathBuf,
    /// Reserved: ack throttle state (last ack time per sender).
    pub(crate) last_message_time: Arc<RwLock<HashMap<String, Instant>>>,
    pub(crate) workspace_dir: PathBuf,
    /// Reserved for future re-implementation of real-time agent-progress
    /// updates. Currently unwired in the 0.8.0 port.
    pub(crate) progress_streaming: bool,
}

impl WuKongIMChannel {
    pub fn from_config(
        config: &WuKongIMConfig,
        alias: impl Into<String>,
        workspace_dir: &Path,
        memory: Arc<dyn Memory>,
    ) -> Self {
        let downloads_dir = if config.downloads_dir.starts_with('/') {
            PathBuf::from(&config.downloads_dir)
        } else {
            workspace_dir.join(&config.downloads_dir)
        };
        Self {
            alias: alias.into(),
            ws_url: config.ws_url.clone(),
            uid: config.uid.clone(),
            token: config.token.clone(),
            device_id: config.device_id.clone(),
            device_flag: config.device_flag,
            allowed_users: config.allowed_users.clone(),
            approval_timeout_secs: config.approval_timeout_secs,
            mention_only: config.mention_only,
            dawn_url: config.dawn_url.clone(),
            dawn_token: config.dawn_token.clone(),
            ack_reactions: config.ack_reactions,
            ack_reactions_message: if config.ack_reactions_message.is_empty() {
                "👌 收到，我想想...".to_string()
            } else {
                config.ack_reactions_message.clone()
            },
            ack_reactions_delay_secs: config.ack_reactions_delay,
            memory,
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            ws_sink: Arc::new(RwLock::new(None)),
            downloads_dir,
            last_message_time: Arc::new(RwLock::new(HashMap::new())),
            workspace_dir: workspace_dir.to_path_buf(),
            progress_streaming: config.progress_streaming,
        }
    }

    async fn send_rpc<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> anyhow::Result<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = Uuid::new_v4().to_string();
        let req = JsonRpcRequest {
            jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
            method: method.to_string(),
            id: id.clone(),
            params,
        };
        self.pending_responses.write().await.insert(id.clone(), tx);
        let send_result: anyhow::Result<()> = async {
            let msg = serde_json::to_string(&req)?;
            let mut g = self.ws_sink.write().await;
            match g.as_mut() {
                Some(s) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                            .with_attrs(::serde_json::json!({"method": method, "id": id})),
                        "WuKongIM: RPC send"
                    );
                    s.send(WsMsg::Text(msg.into())).await?;
                    Ok(())
                }
                None => anyhow::bail!("WuKongIM: WebSocket not connected"),
            }
        }
        .await;
        if let Err(e) = send_result {
            self.pending_responses.write().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(val)) => {
                let resp: JsonRpcResponse<R> = serde_json::from_value(val)?;
                if let Some(err) = resp.error {
                    anyhow::bail!("WuKongIM RPC error: {} (code {})", err.message, err.code);
                }
                resp.result
                    .ok_or_else(|| anyhow::Error::msg("WuKongIM RPC: missing result"))
            }
            Ok(Err(_)) => {
                self.pending_responses.write().await.remove(&id);
                anyhow::bail!("WuKongIM RPC: response channel closed for {}", method);
            }
            Err(_) => {
                self.pending_responses.write().await.remove(&id);
                anyhow::bail!("WuKongIM RPC timeout: {}", method);
            }
        }
    }

    async fn send_ack(&self, message_id: String, message_seq: u32) -> anyhow::Result<()> {
        let req = JsonRpcNotification {
            jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
            method: "recvack".to_string(),
            params: RecvAckParams {
                message_id,
                message_seq,
            },
        };
        let msg = serde_json::to_string(&req)?;
        let mut g = self.ws_sink.write().await;
        if let Some(s) = g.as_mut() {
            s.send(WsMsg::Text(msg.into())).await?;
        }
        Ok(())
    }

    fn get_sync_state_path(&self) -> PathBuf {
        self.workspace_dir
            .join(format!("wukongim_sync_{}.json", self.alias))
    }

    async fn load_sync_state(&self) -> SyncState {
        let path = self.get_sync_state_path();
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            SyncState::default()
        }
    }

    async fn save_sync_state(&self, state: &SyncState) -> anyhow::Result<()> {
        let path = self.get_sync_state_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(state)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    async fn update_sync_state(
        &self,
        channel_id: &str,
        channel_type: u8,
        seq: u32,
        timestamp_ns: i64,
    ) -> anyhow::Result<()> {
        let mut state = self.load_sync_state().await;
        let mut changed = false;

        let seq_key = format!("{channel_id}:{channel_type}");
        let current_seq = *state.channel_seqs.get(&seq_key).unwrap_or(&0);
        if seq > current_seq {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"seq_key": seq_key, "from": current_seq, "to": seq})
                    ),
                "WuKongIM: updating sequence"
            );
            state.channel_seqs.insert(seq_key, seq);
            changed = true;
        }

        if timestamp_ns > state.max_version {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"from": state.max_version, "to": timestamp_ns})
                    ),
                "WuKongIM: updating max_version"
            );
            state.max_version = timestamp_ns;
            changed = true;
        }

        if changed {
            self.save_sync_state(&state).await?;
        }

        Ok(())
    }

    async fn clear_unread(
        &self,
        channel_id: &str,
        channel_type: u8,
        message_seq: u32,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/v1/conversations/clear_unread",
            self.dawn_url.trim_end_matches('/')
        );
        let client = reqwest::Client::new();
        let req = ClearUnreadRequest {
            uid: self.uid.clone(),
            channel_id: channel_id.to_string(),
            channel_type,
            message_seq,
        };

        let resp = client
            .put(&url)
            .header("X-Assistant-Token", &self.dawn_token)
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "channel_id": channel_id,
                        "channel_type": channel_type,
                        "status": resp.status().as_u16(),
                        "url": url,
                    })),
                "WuKongIM: failed to clear unread"
            );
        }
        Ok(())
    }

    async fn sync_history(&self) -> anyhow::Result<Vec<RecvNotificationParams>> {
        let state = self.load_sync_state().await;
        let version = state.max_version;
        let last_msg_seqs = state
            .channel_seqs
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join("|");

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start).with_attrs(
                ::serde_json::json!({"version": version, "last_msg_seqs": last_msg_seqs})
            ),
            "WuKongIM: starting history sync"
        );

        let url = format!(
            "{}/v1/conversations/sync",
            self.dawn_url.trim_end_matches('/')
        );
        let client = reqwest::Client::new();
        let req = SyncRequest {
            uid: self.uid.clone(),
            version,
            last_msg_seqs,
            msg_count: 50,
        };

        let resp = client
            .post(&url)
            .header("X-Assistant-Token", &self.dawn_token)
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("WuKongIM sync failed: status={}", resp.status());
        }

        let body_text = resp.text().await?;
        let sync_resp: SyncResponse = match serde_json::from_str(&body_text) {
            Ok(r) => r,
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string(), "body": body_text})),
                    "WuKongIM: failed to decode sync response"
                );
                anyhow::bail!("WuKongIM: sync decode error: {}", e);
            }
        };
        let mut all_history = Vec::new();
        let mut total_messages = 0;
        let num_conversations = sync_resp.conversations.len();
        for conv in sync_resp.conversations {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "channel_id": conv.channel_id,
                        "channel_type": conv.channel_type,
                        "last_seq": conv.last_msg_seq,
                        "version": conv.version,
                    })),
                "WuKongIM: syncing conversation"
            );
            if let Some(messages) = conv.recents {
                total_messages += messages.len();
                for m in messages {
                    let msg_id = if m.message_id.is_string() {
                        m.message_id.as_str().unwrap_or_default().to_string()
                    } else {
                        m.message_id.to_string()
                    };

                    all_history.push(RecvNotificationParams {
                        message_id: msg_id.clone(),
                        message_seq: m.message_seq,
                        from_uid: m.from_uid.clone(),
                        channel_id: conv.channel_id.clone(),
                        channel_type: conv.channel_type,
                        payload: m.payload.clone(),
                        timestamp: m.timestamp,
                    });

                    let payload_json: Option<serde_json::Value> = if m.payload.is_string() {
                        base64::engine::general_purpose::STANDARD
                            .decode(m.payload.as_str().unwrap_or_default())
                            .ok()
                            .and_then(|d| serde_json::from_slice(&d).ok())
                    } else {
                        Some(m.payload.clone())
                    };

                    let summary = if let Some(pj) = payload_json {
                        if let Some(text) = pj.get("content").and_then(|v| v.as_str()) {
                            text.chars().take(50).collect::<String>()
                        } else {
                            format!(
                                "type={}",
                                pj.get("type").and_then(|v| v.as_i64()).unwrap_or(0)
                            )
                        }
                    } else {
                        "unparseable_payload".to_string()
                    };
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(
                                ::serde_json::json!({"from_uid": m.from_uid, "summary": summary})
                            ),
                        "WuKongIM: history entry"
                    );
                }
            }
            self.update_sync_state(
                &conv.channel_id,
                conv.channel_type,
                conv.last_msg_seq,
                conv.version,
            )
            .await?;
        }

        all_history.sort_by_key(|m| m.timestamp);
        if num_conversations == 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success),
                "WuKongIM: history sync completed, no new updates from server"
            );
        } else {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "messages": total_messages,
                        "conversations": num_conversations,
                    })),
                "WuKongIM: history sync completed"
            );
        }
        Ok(all_history)
    }

    async fn process_inbound_message(
        &self,
        params: RecvNotificationParams,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        if params.from_uid == self.uid {
            return Ok(());
        }

        let seq_key = format!(
            "wukongim:channel_seq:{}:{}:{}",
            self.alias, params.channel_id, params.channel_type
        );
        let current_seq = self
            .memory
            .get(&seq_key)
            .await?
            .and_then(|e| e.content.parse::<u32>().ok())
            .unwrap_or(0);
        if params.message_seq <= current_seq {
            return Ok(());
        }

        if !is_user_allowed(&self.allowed_users, &params.from_uid) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"from_uid": params.from_uid})),
                "WuKongIM: unauthorized sender"
            );
            return Ok(());
        }

        let payload_json: serde_json::Value = if params.payload.is_string() {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(params.payload.as_str().unwrap_or_default())?;
            serde_json::from_slice(&decoded)?
        } else {
            params.payload.clone()
        };

        let msg_type = payload_json
            .get("type")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);

        if msg_type == u64::from(WkMessageType::CMD) || payload_json.get("cmd").is_some() {
            let _ = self
                .send_ack(params.message_id.clone(), params.message_seq)
                .await;
            if payload_json.get("cmd").and_then(|c| c.as_str()) == Some("la_init_helloworld")
                && let Some(content) = payload_json.get("content").and_then(|c| c.as_str())
                && (params.channel_type != WkChannelType::GROUP
                    || is_mentioned(&self.uid, &payload_json, content))
            {
                let target_id = if params.channel_type == WkChannelType::GROUP {
                    &params.channel_id
                } else {
                    &params.from_uid
                };
                let ch_msg = ChannelMessage {
                    id: params.message_id.clone(),
                    sender: target_id.clone(),
                    reply_target: format!("{}:{}", params.channel_type, target_id),
                    content: content.to_string(),
                    channel: "wukongim".to_string(),
                    channel_alias: Some(self.alias.clone()),
                    timestamp: u64::try_from(params.timestamp.max(0)).unwrap_or(0),
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: vec![],
                };
                if tx.send(ch_msg).await.is_ok() {
                    self.update_sync_state(
                        &params.channel_id,
                        params.channel_type,
                        params.message_seq,
                        params.timestamp * 1_000_000_000,
                    )
                    .await?;
                }
            }
            return Ok(());
        }

        if msg_type == u64::from(WkMessageType::INTERACTIVE_RESPONSE) {
            let _ = self
                .send_ack(params.message_id.clone(), params.message_seq)
                .await;
            if let Ok(action) = serde_json::from_value::<WkApprovalAction>(payload_json) {
                let resp = match action.action.as_str() {
                    "approve" => Some(ChannelApprovalResponse::Approve),
                    "deny" => Some(ChannelApprovalResponse::Deny),
                    "always" => Some(ChannelApprovalResponse::AlwaysApprove),
                    _ => None,
                };
                if let Some(r) = resp
                    && let Some(ptx) = self
                        .pending_approvals
                        .write()
                        .await
                        .remove(&action.approval_id)
                {
                    let _ = ptx.send(r);
                }
            }
            return Ok(());
        }

        let mut silent = false;
        let mut final_content_str = if msg_type == u64::from(WkMessageType::MARKDOWN) {
            payload_json
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            payload_json
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string()
        };

        if self.mention_only && params.channel_type == WkChannelType::GROUP {
            let mentioned = is_mentioned(&self.uid, &payload_json, &final_content_str);
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "mentioned": mentioned,
                        "uid": self.uid,
                        "content_len": final_content_str.len(),
                    })),
                "WuKongIM: Group message mention check"
            );

            if !mentioned {
                silent = true;
            } else if !final_content_str.contains(&format!("@{}", self.uid)) {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "WuKongIM: Metadata mention detected, prepending bot UID to force orchestrator reply"
                );
                final_content_str = format!("@{} {}", self.uid, final_content_str);
            }
        }

        let _ = self
            .send_ack(params.message_id.clone(), params.message_seq)
            .await;

        let content = match u32::try_from(msg_type).unwrap_or(0) {
            WkMessageType::IMAGE => {
                let url = payload_json
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                download_image_as_base64(url)
                    .await
                    .unwrap_or_else(|| format!("[图片下载失败]{url}\n请直接描述图片内容"))
            }
            WkMessageType::FILE => {
                let raw_url = payload_json
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                let url = raw_url.split_whitespace().next().unwrap_or(raw_url);
                let name = payload_json
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("文件");
                match download_file_to_workspace(url, &self.downloads_dir, Some(name)).await {
                    Ok(local_path) => format!("[文件]{name}: {local_path}"),
                    Err(err_msg) => format!("[文件]{name}: {url} [下载失败: {err_msg}]"),
                }
            }
            WkMessageType::MARKDOWN => {
                process_markdown_resources(&final_content_str, &self.downloads_dir).await
            }
            _ => final_content_str,
        };

        let target_id = if params.channel_type == WkChannelType::PERSONAL {
            &params.from_uid
        } else {
            &params.channel_id
        };

        let ch_msg = ChannelMessage {
            id: params.message_id,
            sender: target_id.clone(),
            reply_target: format!("{}:{}", params.channel_type, target_id),
            content: if silent {
                format!("<!-- zeroclaw:silent -->{content}")
            } else {
                content
            },
            channel: "wukongim".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp: u64::try_from(params.timestamp.max(0)).unwrap_or(0),
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        if tx.send(ch_msg).await.is_ok() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Dispatch)
                    .with_attrs(::serde_json::json!({
                        "message_seq": params.message_seq,
                        "ts": params.timestamp,
                    })),
                "WuKongIM: message sent to orchestrator, updating sync state"
            );
            self.update_sync_state(
                &params.channel_id,
                params.channel_type,
                params.message_seq,
                params.timestamp * 1_000_000_000,
            )
            .await?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    async fn send_text_message(
        &self,
        channel_id: &str,
        channel_type: u8,
        text: &str,
    ) -> anyhow::Result<()> {
        let payload_b64 = encode_text_payload(text)?;
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id: channel_id.to_string(),
            channel_type,
            payload: serde_json::Value::String(payload_b64),
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }

    async fn process_offline_batch(
        &self,
        messages: Vec<RecvNotificationParams>,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        let mut sorted_messages = messages;
        sorted_messages.sort_by_key(|m| m.timestamp);

        let first = sorted_messages.first().unwrap();
        let last = sorted_messages.last().unwrap();
        let last_seq = last.message_seq;
        let channel_id = first.channel_id.clone();
        let channel_type = first.channel_type;
        let is_group = channel_type == WkChannelType::GROUP;

        let is_silent = if is_group && self.mention_only {
            let mut has_mention = false;
            for m in &sorted_messages {
                let payload_json: serde_json::Value = if m.payload.is_string() {
                    base64::engine::general_purpose::STANDARD
                        .decode(m.payload.as_str().unwrap_or_default())
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or_default()
                } else {
                    m.payload.clone()
                };
                let content = payload_json
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default();

                if is_mentioned(&self.uid, &payload_json, content) {
                    has_mention = true;
                    break;
                }
            }
            !has_mention
        } else {
            false
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "channel_id": channel_id,
                    "channel_type": channel_type,
                    "count": sorted_messages.len(),
                    "is_silent": is_silent,
                })
            ),
            "WuKongIM: processing offline batch"
        );

        self.send_offline_batch_as_single_message(sorted_messages, is_silent, tx)
            .await?;

        self.clear_unread(&channel_id, channel_type, last_seq)
            .await?;
        Ok(())
    }

    async fn send_offline_batch_as_single_message(
        &self,
        messages: Vec<RecvNotificationParams>,
        silent: bool,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        let mut sorted = messages;
        sorted.sort_by_key(|m| m.timestamp);

        let first = sorted.first().unwrap();
        let last = sorted.last().unwrap();
        let channel_id = &first.channel_id;
        let channel_type = first.channel_type;
        let target_id = if channel_type == WkChannelType::PERSONAL {
            &first.from_uid
        } else {
            channel_id
        };

        let mut lines: Vec<String> = Vec::with_capacity(sorted.len());
        for m in &sorted {
            let payload_json: serde_json::Value = if m.payload.is_string() {
                base64::engine::general_purpose::STANDARD
                    .decode(m.payload.as_str().unwrap_or_default())
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default()
            } else {
                m.payload.clone()
            };
            let msg_type = payload_json
                .get("type")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let content = match u32::try_from(msg_type).unwrap_or(0) {
                WkMessageType::IMAGE => {
                    let url = payload_json
                        .get("url")
                        .and_then(|u| u.as_str())
                        .unwrap_or_default();
                    match download_image_as_base64(url).await {
                        Some(data) => format!("[图片]{data}"),
                        None => format!("[图片下载失败]{url}"),
                    }
                }
                WkMessageType::FILE => {
                    let url = payload_json
                        .get("url")
                        .and_then(|u| u.as_str())
                        .unwrap_or_default();
                    let name = payload_json
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("文件");
                    match download_file_to_workspace(url, &self.downloads_dir, Some(name)).await {
                        Ok(local_path) => format!("[文件]{name}: {local_path}"),
                        Err(err) => format!("[文件]{name}: {url} [下载失败: {err}]"),
                    }
                }
                WkMessageType::MARKDOWN => payload_json
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string(),
                _ => payload_json
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string(),
            };
            lines.push(format!("[{}] {}", m.from_uid, content));
        }
        let combined_content = lines.join("\n");

        let ch_msg = ChannelMessage {
            id: first.message_id.clone(),
            sender: target_id.clone(),
            reply_target: format!("{channel_type}:{target_id}"),
            content: if silent {
                format!("<!-- zeroclaw:silent -->{combined_content}")
            } else {
                combined_content
            },
            channel: "wukongim".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp: u64::try_from(last.timestamp.max(0)).unwrap_or(0),
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        if tx.send(ch_msg).await.is_ok() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Dispatch)
                    .with_attrs(::serde_json::json!({
                        "silent": silent,
                        "channel_id": channel_id,
                        "channel_type": channel_type,
                        "seq": last.message_seq,
                    })),
                "WuKongIM: offline batch sent, updating sync state"
            );
            self.update_sync_state(
                channel_id,
                channel_type,
                last.message_seq,
                last.timestamp * 1_000_000_000,
            )
            .await?;
        }

        Ok(())
    }
}

impl Attributable for WuKongIMChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::WuKongIm)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for WuKongIMChannel {
    fn name(&self) -> &str {
        "wukongim"
    }

    fn self_handle(&self) -> Option<String> {
        Some(self.uid.clone())
    }

    fn self_addressed_mention(&self) -> Option<String> {
        Some(format!("@{}", self.uid))
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let content = match message.content.as_str() {
            "ERR:context_window_exceeded" => "⚠️ 模型服务暂时遇到问题，请稍后重试。",
            other => other,
        };
        let payload_b64 = encode_text_payload(content)?;
        let (channel_id, channel_type) = parse_recipient(&message.recipient);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: serde_json::Value::String(payload_b64),
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }

    /// Ephemeral progress text rendered as a short chat message.
    ///
    /// WuKongIM lacks a Slack-style assistant-status banner and JSON-RPC
    /// `send` is the only push primitive we have. To keep these updates
    /// out of message history (so they don't bloat replay / sync), we set
    /// `noPersist = true` and `redDot = false` so:
    ///   * the message is broadcast to currently-connected clients but
    ///     never persisted server-side, and
    ///   * it doesn't bump the chat's unread counter.
    ///
    /// `message_id` is ignored — we always send a fresh ephemeral message
    /// (no edit semantics on the JSON-RPC channel). The 💭 prefix exists
    /// so the user can visually distinguish progress from real responses.
    async fn update_draft_progress(
        &self,
        recipient: &str,
        _message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let content = format!("💭 {trimmed}");
        let payload_b64 = encode_text_payload(&content)?;
        let (channel_id, channel_type) = parse_recipient(recipient);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: serde_json::Value::String(payload_b64),
            header: Some(Header {
                no_persist: Some(true),
                red_dot: Some(false),
                ..Default::default()
            }),
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let history = self.sync_history().await.unwrap_or_else(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"err": e.to_string()})),
                "WuKongIM: history sync failed"
            );
            vec![]
        });

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Connect)
                .with_attrs(::serde_json::json!({"ws_url": self.ws_url})),
            "WuKongIM: connecting"
        );
        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.ws_url).await?;
        let (write, mut read) = ws_stream.split();
        *self.ws_sink.write().await = Some(write);

        {
            let connect_id = Uuid::new_v4().to_string();
            let req = JsonRpcRequest {
                jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                method: "connect".to_string(),
                id: connect_id,
                params: ConnectParams {
                    uid: self.uid.clone(),
                    token: self.token.clone(),
                    device_id: self.device_id.clone(),
                    device_flag: self.device_flag,
                    version: Some(2),
                },
            };
            let msg = serde_json::to_string(&req)?;
            if let Some(s) = self.ws_sink.write().await.as_mut() {
                s.send(WsMsg::Text(msg.into())).await?;
            }
            let connack = tokio::time::timeout(Duration::from_secs(15), read.next())
                .await
                .map_err(|_| anyhow::Error::msg("WuKongIM: connect timeout"))?
                .ok_or_else(|| anyhow::Error::msg("WuKongIM: stream closed during connect"))??;
            if let WsMsg::Text(text) = connack {
                let val: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(err) = val.get("error").filter(|e| !e.is_null()) {
                    anyhow::bail!("WuKongIM: connect rejected: {}", err);
                }
            }
        }
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Connect)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({"uid": self.uid})),
            "WuKongIM: connected"
        );

        // Process offline history (now that WS is connected, agent can reply).
        // Spawn each batch — process_inbound_message awaits RPC responses
        // that are drained by the read loop below.
        let mut grouped: HashMap<(String, u8), Vec<RecvNotificationParams>> = HashMap::new();
        for msg in history {
            let key = (msg.channel_id.clone(), msg.channel_type);
            grouped.entry(key).or_default().push(msg);
        }

        for ((channel_id, channel_type), messages) in grouped {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
                    .with_attrs(::serde_json::json!({
                        "channel_id": channel_id,
                        "channel_type": channel_type,
                        "count": messages.len(),
                    })),
                "WuKongIM: processing offline batch for channel"
            );
            if let Err(e) = self.process_offline_batch(messages, &tx).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string()})),
                    "WuKongIM: offline batch processing failed"
                );
            }
        }

        // Live listen loop. INVARIANT: must NOT await any operation that
        // ultimately waits on `pending_responses` — those oneshots are
        // resolved in the `frame = read.next()` arm below, so blocking on
        // one inside the loop would deadlock the channel.
        // `process_inbound_message` is awaited in a spawned task, never inline.
        let mut hb = tokio::time::interval(PING_INTERVAL);
        let mut last_activity = Instant::now();

        loop {
            tokio::select! {
                _ = hb.tick() => {
                    if last_activity.elapsed() > HEARTBEAT_TIMEOUT {
                        anyhow::bail!("WuKongIM: heartbeat timeout");
                    }
                    let ping = JsonRpcRequest {
                        jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                        method: "ping".to_string(),
                        id: Uuid::new_v4().to_string(),
                        params: serde_json::json!({}),
                    };
                    if let Ok(msg) = serde_json::to_string(&ping)
                        && let Some(s) = self.ws_sink.write().await.as_mut()
                    {
                        let _ = s.send(WsMsg::Text(msg.into())).await;
                    }
                }
                frame = read.next() => {
                    let frame = frame.ok_or_else(|| anyhow::Error::msg("WuKongIM: stream closed"))??;
                    last_activity = Instant::now();
                    let WsMsg::Text(text) = frame else { continue; };
                    let val: serde_json::Value = serde_json::from_str(&text)?;

                    if val.get("method").and_then(|m| m.as_str()) == Some("pong") { continue; }

                    let msg_id = val.get("id").and_then(|i| {
                        if i.is_string() { i.as_str().map(str::to_string) }
                        else if i.is_number() { Some(i.to_string()) }
                        else { None }
                    });
                    if let Some(id) = msg_id
                        && let Some(resp_tx) = self.pending_responses.write().await.remove(&id)
                    {
                        let _ = resp_tx.send(val);
                        continue;
                    }

                    if val.get("method").and_then(|m| m.as_str()) != Some("recv") { continue; }
                    let notif: JsonRpcNotification<RecvNotificationParams> = serde_json::from_value(val)?;
                    // Spawn — see INVARIANT note above.
                    let self_clone = self.clone();
                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = self_clone
                            .process_inbound_message(notif.params, &tx_clone)
                            .await
                        {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({"err": e.to_string()})),
                                "WuKongIM: inbound processing failed"
                            );
                        }
                    });
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let Ok(parsed) = reqwest::Url::parse(&self.ws_url) else {
            return false;
        };
        let Some(host) = parsed.host_str() else {
            return false;
        };
        let port = parsed.port_or_known_default().unwrap_or(80);
        tokio::net::TcpStream::connect((host, port)).await.is_ok()
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let approval_id = Uuid::new_v4().to_string();
        let card = build_approval_card(&approval_id, request, self.approval_timeout_secs);
        let payload_b64 =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&card)?);
        let (channel_id, channel_type) = parse_recipient(recipient);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: serde_json::Value::String(payload_b64),
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let (otx, orx) = tokio::sync::oneshot::channel();
        self.pending_approvals
            .write()
            .await
            .insert(approval_id.clone(), otx);
        self.send_rpc::<_, serde_json::Value>("send", params)
            .await?;
        match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), orx).await {
            Ok(Ok(resp)) => Ok(Some(resp)),
            _ => {
                self.pending_approvals.write().await.remove(&approval_id);
                Ok(Some(ChannelApprovalResponse::Deny))
            }
        }
    }
}
