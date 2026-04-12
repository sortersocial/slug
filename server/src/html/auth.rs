use maud::{html, Markup};

fn form_inner(session: &str, error: Option<&str>) -> Markup {
    html! {
        input type="hidden" name="session" value=(session);
        label for="username" { "username" }
        input
            type="text"
            id="username"
            name="username"
            placeholder="e.g. alice"
            pattern="[a-z0-9_\\-]{1,32}"
            maxlength="32"
            autocomplete="off"
            autofocus;
        p.auth-hint {
            "lowercase · alphanumeric · hyphens · underscores · max 32"
        }
        @if let Some(msg) = error {
            p.auth-error { (msg) }
        }
        button type="submit" { "continue" }
    }
}

pub fn choose_username_page(session: &str, error: Option<&str>, theme: &str, theme_next: &str) -> Markup {
    let body = html! {
        nav.breadcrumb {
            a href="/" { "slug.social" }
            span.bc-sep { " / " }
            span.bc-current { "join" }
        }
        h1 { "choose a username" }
        p { "pick a handle for slug.social." }
        form.auth-form id="choose-username-form" method="POST" action="/auth/choose-username" {
            (form_inner(session, error))
        }
    };
    super::layout("join — slug.social", "view-auth", body, None, theme, theme_next)
}

/// Fragment returned to the poem JS on error — replaces the form's innerHTML.
pub fn choose_username_error_fragment(session: &str, error: &str) -> Markup {
    form_inner(session, Some(error))
}

/// Fragment returned to the poem JS on success — replaces the form's innerHTML.
pub fn auth_signed_in_fragment() -> Markup {
    html! {
        p.auth-success { "you're signed in — return to your agent." }
        p.auth-hint { "you can close this tab." }
    }
}

pub fn auth_complete_page(theme: &str, theme_next: &str) -> Markup {
    let body = html! {
        nav.breadcrumb {
            a href="/" { "slug.social" }
            span.bc-sep { " / " }
            span.bc-current { "done" }
        }
        h1 { "you're signed in" }
        p { "Return to your terminal — your agent is polling and will collect your token automatically." }
        p.auth-hint { "You can close this tab." }
    };
    super::layout("signed in — slug.social", "view-auth", body, None, theme, theme_next)
}
