use maud::{html, Markup};

use super::copy::thread_copy_button_markup;
use super::nav::{ThreadNav, THREAD_PAGE_SIZE};

pub(crate) const PAGE_SIZE: usize = THREAD_PAGE_SIZE;

/// Start offset of the page containing the newest post.
///
/// Thread pages are **fixed windows** aligned to multiples of [`PAGE_SIZE`]
/// (`0..10`, `10..20`, …), so appending a post never shifts posts already on a
/// page — the latest page just grows until it is full, then a new page starts.
pub(crate) fn latest_page_offset(total: usize) -> usize {
    if total == 0 {
        0
    } else {
        ((total - 1) / PAGE_SIZE) * PAGE_SIZE
    }
}

/// Snap a requested `?offset=` value to a valid page boundary for this thread.
pub(crate) fn snap_page_offset(requested: usize, total: usize) -> usize {
    let clamped = requested.min(latest_page_offset(total));
    clamped - (clamped % PAGE_SIZE)
}

/// Prev/next links between chronological posts on a single-post (`/t/tag/N`) page.
pub(super) fn render_post_permalink_nav(nav: &ThreadNav, tag: &str, index: usize, total: usize) -> Markup {
    let prev_idx = index.checked_sub(1);
    let next_idx = if index + 1 < total { Some(index + 1) } else { None };
    html! {
        div class="post-nav" {
            @if let Some(i) = prev_idx {
                a href=(nav.post_url(tag, i)) class="post-nav-btn" { "← previous" }
            } @else {
                a href="#" class="post-nav-btn disabled" { "← previous" }
            }
            span class="post-nav-pos muted" {
                (index + 1) " / " (total)
            }
            @if let Some(i) = next_idx {
                a href=(nav.post_url(tag, i)) class="post-nav-btn" { "next →" }
            } @else {
                a href="#" class="post-nav-btn disabled" { "next →" }
            }
        }
    }
}

pub(super) fn render_thread_paginator(nav: &ThreadNav, tag: &str, offset: usize, total: usize, top: bool) -> Markup {
    let newer_offset = offset.checked_add(PAGE_SIZE).filter(|&o| o < total);
    let older_offset = if offset > 0 {
        Some(offset.saturating_sub(PAGE_SIZE))
    } else {
        None
    };
    let latest_offset = latest_page_offset(total);
    let on_latest = offset >= latest_offset;
    let (id, scroll_href, scroll_label) = if top {
        ("top", "#bottom", "↓")
    } else {
        ("bottom", "#top", "↑")
    };
    html! {
        div class="thread-paginator" id=(id) {
            a href=(scroll_href) class="post-nav-btn" { (scroll_label) }
            @if let Some(o) = older_offset {
                a href=(nav.thread_page_url(tag, o)) class="post-nav-btn" { "← older" }
            } @else {
                a href="#" class="post-nav-btn disabled" { "← older" }
            }
            span class="post-nav-pos muted" {
                (offset + 1) "–" (total.min(offset + PAGE_SIZE)) " / " (total)
            }
            @if let Some(o) = newer_offset {
                a href=(nav.thread_page_url(tag, o)) class="post-nav-btn" { "newer →" }
            } @else {
                a href="#" class="post-nav-btn disabled" { "newer →" }
            }
            @if !on_latest {
                a href=(nav.thread_page_url(tag, latest_offset)) class="post-nav-btn" { "latest" }
            }
            (thread_copy_button_markup(nav, tag, top))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_page_offset_is_page_aligned() {
        assert_eq!(latest_page_offset(0), 0);
        assert_eq!(latest_page_offset(1), 0);
        assert_eq!(latest_page_offset(PAGE_SIZE), 0);
        assert_eq!(latest_page_offset(PAGE_SIZE + 1), PAGE_SIZE);
        assert_eq!(latest_page_offset(2 * PAGE_SIZE), PAGE_SIZE);
        assert_eq!(latest_page_offset(2 * PAGE_SIZE + 1), 2 * PAGE_SIZE);
    }

    #[test]
    fn snap_page_offset_clamps_and_aligns() {
        assert_eq!(snap_page_offset(0, 0), 0);
        assert_eq!(snap_page_offset(7, 25), 0);
        assert_eq!(snap_page_offset(PAGE_SIZE + 5, 25), PAGE_SIZE);
        assert_eq!(snap_page_offset(999, 25), 2 * PAGE_SIZE);
        assert_eq!(snap_page_offset(PAGE_SIZE, 5), 0);
    }
}
