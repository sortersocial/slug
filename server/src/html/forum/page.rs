use axum::http::HeaderMap;
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};

use crate::api::optional_principal;
use crate::reducer::ReducerState;

use super::nav::ThreadNav;
use crate::html::bc_segment;

pub(super) fn auth_strip(
    headers: &HeaderMap,
    jar: &CookieJar,
    reduced: &ReducerState,
) -> Markup {
    match optional_principal(headers, jar, reduced) {
        Some(u) => html! {
            p class="muted auth-strip" {
                "@" (u)
                " · "
                a href="/logout" { "log out" }
            }
        },
        None => html! {
            p class="muted auth-strip" {
                a href="/login" { "log in" }
            }
        },
    }
}

pub(super) fn bc_room(
    nav: &ThreadNav,
    room_slug: &str,
    thread_tag: Option<&str>,
    focused_post: Option<usize>,
) -> Markup {
    html! {
        a href="/" { "slug.social" }
        @if let Some(t) = thread_tag {
            (bc_segment(
                &format!("room:{room_slug}"),
                nav.room_url(),
                false,
            ))
            @if let Some(idx) = focused_post {
                (bc_segment(&format!("#{t}"), &nav.thread_url(t), false))
                (bc_segment(&format!("post #{idx}"), &nav.post_url(t, idx), true))
            } @else {
                (bc_segment(&format!("#{t}"), &nav.thread_url(t), true))
            }
        } @else {
            (bc_segment(
                &format!("room:{room_slug}"),
                nav.room_url(),
                true,
            ))
        }
    }
}
