//! GET /api/approvals/grants + DELETE /api/approvals/grants/{id}

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zeroclaw_runtime::approval::{ApprovalGrant, ApprovalGrantStore, GrantFilter};

#[derive(Clone)]
pub struct ApprovalsState {
    pub grants: Arc<dyn ApprovalGrantStore>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub channel: Option<String>,
    pub topic: Option<String>,
    /// Use `topic=__none__` to filter the no-topic bucket.
    pub user: Option<String>,
    pub tool: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeleteResp {
    deleted: bool,
}

async fn list_grants(
    State(s): State<ApprovalsState>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let filter = GrantFilter {
        channel_ref: q.channel,
        topic: q
            .topic
            .map(|t| if t == "__none__" { None } else { Some(t) }),
        user_master_id: q.user,
        tool_name: q.tool,
    };
    match s.grants.list(&filter) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "list failed"})),
        )
            .into_response(),
    }
}

async fn delete_grant(
    State(s): State<ApprovalsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.grants.delete(&id) {
        Ok(true) => (StatusCode::OK, Json(DeleteResp { deleted: true })).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(DeleteResp { deleted: false })).into_response(),
        Err(_e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "delete failed"})),
        )
            .into_response(),
    }
}

pub fn router(state: ApprovalsState) -> Router {
    Router::new()
        .route("/api/approvals/grants", get(list_grants))
        .route("/api/approvals/grants/{id}", delete(delete_grant))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_runtime::approval::SqliteGrantStore;

    fn state() -> (TempDir, ApprovalsState) {
        let tmp = TempDir::new().unwrap();
        let grants =
            Arc::new(SqliteGrantStore::new(tmp.path()).unwrap()) as Arc<dyn ApprovalGrantStore>;
        (tmp, ApprovalsState { grants })
    }

    #[tokio::test]
    async fn list_empty_returns_empty_array() {
        let (_t, st) = state();
        let app = router(st);
        let resp = axum::body::to_bytes(
            tower::ServiceExt::oneshot(
                app,
                axum::http::Request::builder()
                    .uri("/api/approvals/grants")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
            usize::MAX,
        )
        .await
        .unwrap();
        let v: Vec<ApprovalGrant> = serde_json::from_slice(&resp).unwrap();
        assert!(v.is_empty());
    }

    #[tokio::test]
    async fn delete_missing_returns_404() {
        let (_t, st) = state();
        let app = router(st);
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("DELETE")
                .uri("/api/approvals/grants/nope")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_then_list_then_delete_round_trip() {
        let (_t, st) = state();
        let g = ApprovalGrant::new(
            "dawnim.work".into(),
            Some("t1".into()),
            "u_alice".into(),
            "shell".into(),
            "u_admin".into(),
            "dawnim.work".into(),
        );
        let id = g.id.clone();
        st.grants.put(g).unwrap();

        let app = router(st.clone());
        let body = axum::body::to_bytes(
            tower::ServiceExt::oneshot(
                app.clone(),
                axum::http::Request::builder()
                    .uri("/api/approvals/grants?channel=dawnim.work")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
            usize::MAX,
        )
        .await
        .unwrap();
        let v: Vec<ApprovalGrant> = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.len(), 1);

        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/approvals/grants/{id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
