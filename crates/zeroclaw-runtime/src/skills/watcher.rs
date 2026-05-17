//! File system watcher for skills directory hot reload.
//!
//! Uses notify crate for cross-platform file system monitoring,
//! with fallback to polling mode when notify is unavailable.

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher, Event, EventKind, Config};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn, error, debug};

/// Configuration for the skill watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Directory to watch for skill changes
    pub skills_dir: PathBuf,

    /// Debounce delay in milliseconds
    pub debounce_ms: u64,

    /// Watch mode: "notify", "poll", or "off"
    pub watch_mode: String,

    /// Poll interval in seconds (for poll mode)
    pub poll_interval_secs: u64,
}

impl WatcherConfig {
    pub fn from_config(
        skills_dir: PathBuf,
        hot_reload_config: &zeroclaw_config::schema::SkillsHotReloadConfig,
    ) -> Self {
        Self {
            skills_dir,
            debounce_ms: hot_reload_config.debounce_ms,
            watch_mode: hot_reload_config.watch_mode.clone(),
            poll_interval_secs: hot_reload_config.poll_interval_secs,
        }
    }
}

/// Request to reload skills.
#[derive(Debug, Clone)]
pub struct ReloadRequest {
    /// If true, force reload even if debounce hasn't elapsed
    pub force: bool,
}

/// Spawn a background task that watches the skills directory and sends
/// reload requests when changes are detected.
///
/// Returns a JoinHandle for the spawned task and a Receiver for
/// ReloadRequest messages.
pub fn spawn_skill_watcher(
    config: WatcherConfig,
) -> Result<(tokio::task::JoinHandle<()>, mpsc::Receiver<ReloadRequest>)> {
    let (reload_tx, reload_rx) = mpsc::channel(4);

    if config.watch_mode == "off" {
        info!("Skill hot reload disabled by config");
        let handle = tokio::spawn(async move {
            // Keep the task alive but do nothing
            debug!("Skill watcher task running but disabled");
        });
        return Ok((handle, reload_rx));
    }

    let handle = if config.watch_mode == "poll" {
        spawn_poll_watcher(config, reload_tx)?
    } else {
        spawn_notify_watcher(config, reload_tx)?
    };

    Ok((handle, reload_rx))
}

/// Spawn watcher using notify crate (preferred mode).
fn spawn_notify_watcher(
    config: WatcherConfig,
    reload_tx: mpsc::Sender<ReloadRequest>,
) -> Result<tokio::task::JoinHandle<()>> {
    let skills_dir = config.skills_dir.clone();

    info!("Spawning notify watcher for skills directory: {:?} (watch_mode: {})", skills_dir, config.watch_mode);

    if !skills_dir.exists() {
        warn!(
            "Skills directory does not exist: {:?}. Will retry when created.",
            skills_dir
        );
    }

    let handle = tokio::spawn(async move {
        if let Err(e) = run_notify_watcher(config, reload_tx).await {
            error!("Notify watcher failed: {}", e);
        }
    });

    Ok(handle)
}

/// Run the notify-based watcher.
async fn run_notify_watcher(
    config: WatcherConfig,
    reload_tx: mpsc::Sender<ReloadRequest>,
) -> Result<()> {
    use std::sync::Mutex;

    let (event_tx, mut event_rx) = mpsc::channel(32);
    let event_tx = Arc::new(Mutex::new(event_tx));

    let mut watcher = RecommendedWatcher::new(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            // Filter for relevant event kinds (create, modify, remove)
            if matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                if let Err(_) = event_tx.lock().unwrap().try_send(event) {
                    debug!("Watcher event channel full, dropping event");
                }
            }
        }
    }, Config::default())
    .context("Failed to create file watcher")?;

    // Watch the skills directory
    let skills_dir = &config.skills_dir;
    if skills_dir.exists() {
        watcher
            .watch(skills_dir, RecursiveMode::Recursive)
            .context("Failed to watch skills directory")?;
    }

    info!(
        "Watching skills directory: {:?} (debounce: {}ms)",
        skills_dir, config.debounce_ms
    );

    // Debounce loop: wait for a quiet period after the last event
    let debounce_duration = Duration::from_millis(config.debounce_ms);
    let mut pending_event = false;
    let mut last_event_time = Instant::now();

    loop {
        tokio::select! {
            // Receive new file event
            event = event_rx.recv() => {
                match event {
                    Some(event) => {
                        debug!("Got file event: {:?}", event);
                        pending_event = true;
                        last_event_time = Instant::now();
                    }
                    None => {
                        warn!("Watcher event channel closed, stopping");
                        break;
                    }
                }
            }

            // Debounce timer - fire after quiet period
            _ = tokio::time::sleep(debounce_duration), if pending_event => {
                // Check if enough time has passed since last event
                let elapsed = Instant::now().duration_since(last_event_time);
                if elapsed >= debounce_duration {
                    info!("Skills directory changed, triggering reload (debounced)");
                    if reload_tx.send(ReloadRequest { force: false }).await.is_err() {
                        warn!("Reload request channel closed, stopping watcher");
                        break;
                    }
                    pending_event = false;
                }
            }
        }
    }

    Ok(())
}

/// Spawn watcher using polling (fallback mode).
fn spawn_poll_watcher(
    config: WatcherConfig,
    reload_tx: mpsc::Sender<ReloadRequest>,
) -> Result<tokio::task::JoinHandle<()>> {
    let handle = tokio::spawn(async move {
        if let Err(e) = run_poll_watcher(config, reload_tx).await {
            error!("Poll watcher failed: {}", e);
        }
    });

    Ok(handle)
}

/// Run the polling-based watcher.
async fn run_poll_watcher(
    config: WatcherConfig,
    reload_tx: mpsc::Sender<ReloadRequest>,
) -> Result<()> {
    use std::fs;

    let skills_dir = config.skills_dir;
    let interval = Duration::from_secs(config.poll_interval_secs);

    let mut last_mtimes = std::collections::HashMap::new();

    info!(
        "Polling skills directory: {:?} (interval: {}s)",
        skills_dir, config.poll_interval_secs
    );

    loop {
        tokio::time::sleep(interval).await;

        if !skills_dir.exists() {
            continue;
        }

        let mut changed = false;

        if let Ok(entries) = fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Ok(metadata) = fs::metadata(&path) {
                        if let Ok(modified) = metadata.modified() {
                            let mtime = modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            if let Some(&last_mtime) = last_mtimes.get(&path) {
                                if mtime != last_mtime {
                                    changed = true;
                                }
                            }
                            last_mtimes.insert(path, mtime);
                        }
                    }
                }
            }
        }

        if changed {
            info!("Skills directory changed (detected by poll), triggering reload");
            if reload_tx
                .send(ReloadRequest { force: false })
                .await
                .is_err()
            {
                warn!("Reload request channel closed, stopping watcher");
                break;
            }
        }
    }

    Ok(())
}
