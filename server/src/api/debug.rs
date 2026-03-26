use axum::{response::IntoResponse, Json};
use axum_extra::extract::Query;
use serde::{Deserialize, Serialize};

/// Debug endpoint to prove querystring decoding + repeated params behavior.
///
/// Example:
/// `/debug/query?open=~%2Fmodels%2Fllms&open=~%2Fmusic%2Fjazz&q=hello+world`
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DebugQuery {
    /// Supports repeated `open=` params: `?open=a&open=b`.
    #[serde(default)]
    pub open: Vec<String>,
    /// Optional single selection.
    #[serde(default)]
    pub selected: Option<String>,
    /// A freeform string to show percent-decoding / + decoding.
    #[serde(default)]
    pub q: Option<String>,
}

pub async fn get_debug_query(Query(q): Query<DebugQuery>) -> impl IntoResponse {
    Json(q)
}

