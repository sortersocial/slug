//! Single `POST /ui` entry for browser [`crate::html::ui_action::HtmlUiAction`] (JSON in `__rpc__` + holes).

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use std::collections::HashMap;

use crate::{
    api::{
        auth::optional_principal,
        web_post::{run_check_web_ingest, run_post_web_ingest, run_post_web_redact, WebPostForm, WebRedactForm},
    },
    html::{
        fragment_public_new_thread_form, fragment_room_new_thread_form, login_to_post_hint_markup,
        parse_html_ui_from_form, user_can_post_room, user_can_view_room, HtmlUiAction, JsBuilder,
        ThreadNav,
    },
    state::AppState,
};

pub async fn post_ui_html(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let action = match parse_html_ui_from_form(&form) {
        Ok(a) => a,
        Err(e) => return ui_js_warn(&e.to_string()).into_response(),
    };

    match action {
        HtmlUiAction::PostIngest {
            room,
            thread_tag,
            text,
            error_target,
            form_id,
        } => {
            run_post_web_ingest(
                &state,
                &headers,
                &jar,
                WebPostForm {
                    room,
                    thread_tag,
                    text,
                    error_target,
                    form_id,
                },
            )
            .await
        }
        HtmlUiAction::CheckIngest {
            room,
            thread_tag,
            text,
            error_target,
            form_id,
        } => {
            run_check_web_ingest(
                &state,
                &headers,
                &jar,
                WebPostForm {
                    room,
                    thread_tag,
                    text,
                    error_target,
                    form_id,
                },
            )
            .await
        }
        HtmlUiAction::RedactPost { post_id } => {
            run_post_web_redact(&state, &headers, &jar, WebRedactForm { post_id }).await
        }
        HtmlUiAction::ExpandPublicNewThreadForm => {
            let reduced = state.reduced.read().await;
            let user = optional_principal(&headers, &jar, &reduced);
            drop(reduced);
            let markup = if user.is_some() {
                fragment_public_new_thread_form(true)
            } else {
                login_to_post_hint_markup()
            };
            JsBuilder::new()
                .morph_selector("#public-new-thread-ui-slot", markup)
                .into_response()
        }
        HtmlUiAction::ExpandRoomNewThreadForm { room_wire } => {
            let room_wire = room_wire.trim().to_string();
            if room_wire.is_empty() {
                return ui_js_warn("missing room").into_response();
            }
            let reduced = state.reduced.read().await;
            let user = optional_principal(&headers, &jar, &reduced);
            if !reduced.rooms.contains(&room_wire) {
                drop(reduced);
                return ui_js_warn("room not found").into_response();
            }
            if !user_can_view_room(&reduced, &room_wire, user.as_deref()) {
                drop(reduced);
                return ui_js_warn("forbidden").into_response();
            }
            let can_post = user
                .as_ref()
                .map(|u| user_can_post_room(&reduced, &room_wire, u))
                .unwrap_or(false);
            drop(reduced);
            let Some(nav) = ThreadNav::from_room_id(&room_wire) else {
                return ui_js_warn("bad room").into_response();
            };
            let markup = if can_post {
                fragment_room_new_thread_form(&nav, true)
            } else {
                login_to_post_hint_markup()
            };
            JsBuilder::new()
                .morph_selector("#room-new-thread-ui-slot", markup)
                .into_response()
        }
    }
}

fn ui_js_warn(msg: &str) -> Response {
    use crate::html::js_string_literal;
    let js = format!("console.warn({});", js_string_literal(msg));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        .body(Body::from(js))
        .unwrap()
}
