use crate::form_template::template_json_compact;
use maud::{html, Markup};

use crate::html::ui_action::HtmlUiAction;

use super::nav::ThreadNav;

pub(super) fn thread_graduate_button_markup(nav: &ThreadNav, tag: &str) -> Markup {
    let rpc = template_json_compact(&HtmlUiAction::GraduateThread {
        room: nav.room_wire.clone(),
        thread_tag: tag.to_string(),
    })
    .expect("GraduateThread serializes");
    html! {
        button type="button" class="post-nav-btn graduate-thread-btn" title="Copy this thread to the public forum"
            onclick=(format!(
                "if(confirm('Graduate #{} to the public forum? All posts and garden votes will be copied to public #{}.')){{fetch('/ui',{{method:'POST',headers:{{'Content-Type':'application/x-www-form-urlencoded'}},body:new URLSearchParams({{__rpc__:{}}}).toString(),credentials:'same-origin'}}).then(r=>r.text()).then(eval);}}return false;",
                tag,
                tag,
                crate::html::js_string_literal(&rpc)
            )) {
            "graduate to public"
        }
    }
}

pub(super) fn thread_graduated_banner_markup(tag: &str, public_href: &str) -> Markup {
    html! {
        p class="thread-graduated-banner muted" {
            "This private thread graduated to "
            a href=(public_href) { "#" (tag) }
            " on the public forum. Post there to continue."
        }
    }
}
