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
    // Increased channel capacity to handle burst of file events
    let (event_tx, mut event_rx) = mpsc::channel(64);
    let event_tx = Arc::new(event_tx);
    // Track if we dropped events due to channel overflow
    let dropped_events = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let dropped_events_clone = Arc::clone(&dropped_events);
    let event_tx_clone = Arc::clone(&event_tx);

    let mut watcher = RecommendedWatcher::new(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            // Filter for relevant event kinds (create, modify, remove)
            if matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                if let Err(_) = event_tx_clone.try_send(event) {
                    // Channel full - increment counter and trigger reload later
                    dropped_events_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    debug!("Watcher event channel full, dropping event (will force reload)");
                }
            }
        }
    }, Config::default())
    .context("Failed to create file watcher")?;

    // Watch the skills directory if it exists; otherwise enter a retry loop
    // that periodically checks for directory creation.
    let skills_dir = &config.skills_dir;
    let mut watching = false;
    if skills_dir.exists() {
        watcher
            .watch(skills_dir, RecursiveMode::Recursive)
            .context("Failed to watch skills directory")?;
        watching = true;
        info!(
            "Watching skills directory: {:?} (debounce: {}ms)",
            skills_dir, config.debounce_ms
        );
    } else {
        warn!(
            "Skills directory does not exist: {:?}. Will retry every 30s.",
            skills_dir
        );
    }

    // Debounce loop: wait for a quiet period after the last event
    let debounce_duration = Duration::from_millis(config.debounce_ms);
    let dir_retry_interval = Duration::from_secs(30);
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
                    // Check if we dropped events - force reload if so
                    let dropped = dropped_events.swap(0, std::sync::atomic::Ordering::Relaxed);
                    let force = dropped > 0;
                    if force {
                        warn!("{} events were dropped, forcing reload", dropped);
                    }
                    info!("Skills directory changed, triggering reload (debounced, force={})", force);
                    if reload_tx.send(ReloadRequest { force }).await.is_err() {
                        warn!("Reload request channel closed, stopping watcher");
                        break;
                    }
                    pending_event = false;
                }
            }

            // Directory existence retry — when the skills dir didn't exist at
            // startup, periodically check whether it has been created so we can
            // register the watcher.
            _ = tokio::time::sleep(dir_retry_interval), if !watching => {
                if skills_dir.exists() {
                    match watcher.watch(skills_dir, RecursiveMode::Recursive) {
                        Ok(()) => {
                            watching = true;
                            info!(
                                "Skills directory appeared, now watching: {:?}",
                                skills_dir
                            );
                            // Trigger an immediate reload to pick up any existing skills
                            if reload_tx.send(ReloadRequest { force: true }).await.is_err() {
                                warn!("Reload request channel closed, stopping watcher");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Skills directory found but watch failed: {}. Retrying in 30s.",
                                e
                            );
                        }
                    }
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
    use std::collections::HashSet;
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
        let mut current_paths = HashSet::new();

        if let Ok(entries) = fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    current_paths.insert(path.clone());
                    if let Ok(metadata) = fs::metadata(&path) {
                        if let Ok(modified) = metadata.modified() {
                            let mtime = modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            match last_mtimes.get(&path) {
                                // Modified: mtime changed since last poll
                                Some(&last_mtime) if mtime != last_mtime => {
                                    changed = true;
                                }
                                // New directory: not seen in previous poll
                                None => {
                                    changed = true;
                                }
                                _ => {}
                            }
                            last_mtimes.insert(path, mtime);
                        }
                    }
                }
            }
        }

        // Detect removed directories (paths in last_mtimes but not on disk)
        for path in last_mtimes.keys() {
            if !current_paths.contains(path) {
                changed = true;
                break;
            }
        }

        // Remove stale entries for directories that no longer exist
        last_mtimes.retain(|path, _| current_paths.contains(path));

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
