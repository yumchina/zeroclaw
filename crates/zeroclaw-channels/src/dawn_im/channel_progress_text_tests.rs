use super::to_cn_progress_text;
use zeroclaw_api::channel::{ProgressPhase, ProgressUpdate};

#[test]
fn agent_start_is_cn_without_cloud_prefix() {
    let update = ProgressUpdate {
        text: "Agent started (custom.nextg_std/nextg-std)".into(),
        phase: ProgressPhase::AgentStart {
            provider: "custom.nextg_std".into(),
            model: "nextg-std".into(),
        },
    };
    assert_eq!(
        to_cn_progress_text(&update),
        "Agent 启动（custom.nextg_std/nextg-std）"
    );
}

#[test]
fn agent_end_is_cn_done() {
    let update = ProgressUpdate {
        text: "Done".into(),
        phase: ProgressPhase::AgentEnd,
    };
    assert_eq!(to_cn_progress_text(&update), "处理完成");
}

#[test]
fn generic_done_and_agent_started_are_mapped_to_cn() {
    let done = ProgressUpdate {
        text: "💭 Done".into(),
        phase: ProgressPhase::Generic,
    };
    assert_eq!(to_cn_progress_text(&done), "处理完成");

    let started = ProgressUpdate {
        text: "💭 Agent started (custom.nextg_std/nextg-std)".into(),
        phase: ProgressPhase::Generic,
    };
    assert_eq!(
        to_cn_progress_text(&started),
        "Agent 启动（custom.nextg_std/nextg-std）"
    );
}
