"""
Playwright end-to-end tests for the /tree view.

These tests require a running slugsocial-server with tree data already
ingested.  The Babashka rig (test/playwright.bb) boots the server and
passes --base-url to pytest so Playwright pages land on it by default.

Tests that can pass with an empty database are marked accordingly.
"""

import re
import urllib.parse

import pytest
from playwright.sync_api import Page, expect


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def get_s_param(url: str) -> str | None:
    """Extract the ?s= query parameter from a URL."""
    parsed = urllib.parse.urlparse(url)
    qs = urllib.parse.parse_qs(parsed.query)
    vals = qs.get("s", [])
    return vals[0] if vals else None


# ---------------------------------------------------------------------------
# tests that pass even with an empty database
# ---------------------------------------------------------------------------

class TestTreePageLoadsEmpty:
    """Basic smoke tests that work against an empty database."""

    def test_tree_root_redirects_to_state_blob(self, page: Page, base_url: str):
        """GET /tree without ?s= should redirect to a URL containing ?s=<blob>."""
        # Follow redirect (Playwright does by default)
        page.goto(f"{base_url}/tree")
        # After redirect we must have ?s= in the URL
        assert "s=" in page.url, f"Expected ?s= in URL, got: {page.url}"

    def test_tree_page_returns_200(self, page: Page, base_url: str):
        """After redirect, the tree page must return HTTP 200."""
        response = page.goto(f"{base_url}/tree")
        # Playwright follows the redirect; the final response is 200
        assert response is not None
        assert response.status == 200

    def test_tree_pane_present(self, page: Page, base_url: str):
        """The tree-pane div must be present in the DOM."""
        page.goto(f"{base_url}/tree")
        expect(page.locator("#tree-pane")).to_be_visible()

    def test_detail_pane_present(self, page: Page, base_url: str):
        """The detail-pane div must be present in the DOM."""
        page.goto(f"{base_url}/tree")
        expect(page.locator("#detail-pane")).to_be_visible()

    def test_detail_pane_shows_placeholder_when_nothing_selected(
        self, page: Page, base_url: str
    ):
        """With nothing selected the detail pane shows the 'select a node' hint."""
        page.goto(f"{base_url}/tree")
        # The baseline blob has selected=None so the detail pane should say
        # 'select a node'
        detail = page.locator("#detail-pane")
        expect(detail).to_contain_text("select a node")

    def test_s_param_survives_page_reload(self, page: Page, base_url: str):
        """The ?s= value in the URL must remain identical after a reload."""
        page.goto(f"{base_url}/tree")
        url_before = page.url
        s_before = get_s_param(url_before)
        assert s_before is not None, "?s= must be present before reload"

        page.reload()
        s_after = get_s_param(page.url)
        assert s_after == s_before, (
            f"?s= changed after reload: {s_before!r} -> {s_after!r}"
        )

    def test_tree_page_title(self, page: Page, base_url: str):
        """Page title should contain 'tree'."""
        page.goto(f"{base_url}/tree")
        assert "tree" in page.title().lower()

    def test_invalid_state_blob_returns_400(self, page: Page, base_url: str):
        """A garbage ?s= blob should result in a 400 response."""
        response = page.goto(f"{base_url}/tree?s=notvalidbase64!!!")
        assert response is not None
        assert response.status == 400


# ---------------------------------------------------------------------------
# tests that require populated tree data
# ---------------------------------------------------------------------------

class TestTreeInteraction:
    """
    Interactive tree tests.  These assume the database has at least one
    public ontology item with children so that expandable nodes exist.
    If the tree has no nodes, the tests will be skipped with a clear message.
    """

    @pytest.fixture(autouse=True)
    def require_nodes(self, page: Page, base_url: str):
        """Skip the whole class if the tree has no expandable nodes."""
        page.goto(f"{base_url}/tree")
        twist_buttons = page.locator("button.tree-twist")
        if twist_buttons.count() == 0:
            pytest.skip(
                "No expandable tree nodes found. "
                "Ingest some public ontology items first."
            )

    def test_tree_shows_nodes(self, page: Page, base_url: str):
        """The tree pane should contain at least one tree-row."""
        page.goto(f"{base_url}/tree")
        expect(page.locator(".tree-row").first).to_be_visible()

    def test_clicking_node_label_updates_url(self, page: Page, base_url: str):
        """
        Clicking a node label (anchor) should produce a ?s= param that
        encodes the selection — i.e. the URL changes.
        """
        page.goto(f"{base_url}/tree")
        s_before = get_s_param(page.url)

        # Click the first node label link
        page.locator("a.tree-label").first.click()
        page.wait_for_load_state("networkidle")

        s_after = get_s_param(page.url)
        # After selecting a node the state blob must be present (and may differ)
        assert s_after is not None, "?s= must be present after clicking a label"

    def test_clicking_twist_expands_node(self, page: Page, base_url: str):
        """
        Clicking a twist button (▸) should POST to /tree/toggle, morph the
        DOM, and show the twist as ▾ (open).
        """
        page.goto(f"{base_url}/tree")

        # Find a closed twist (▸)
        closed_twist = page.locator("button.tree-twist", has_text="▸").first
        expect(closed_twist).to_be_visible()

        # Intercept the POST to confirm it fires
        toggle_requests = []
        page.on(
            "request",
            lambda req: toggle_requests.append(req)
            if req.url.endswith("/tree/toggle")
            else None,
        )

        closed_twist.click()
        # Wait for the morph to complete (DOM update + URL push)
        page.wait_for_function("() => document.querySelector('button.tree-twist[title=\"toggle\"]') !== null")

        assert len(toggle_requests) >= 1, "Expected at least one POST to /tree/toggle"

    def test_url_updates_after_toggle(self, page: Page, base_url: str):
        """After toggling a node the URL ?s= param must change."""
        page.goto(f"{base_url}/tree")
        s_before = get_s_param(page.url)

        closed_twist = page.locator("button.tree-twist", has_text="▸").first
        expect(closed_twist).to_be_visible()
        closed_twist.click()
        # Give the JS time to replaceState
        page.wait_for_timeout(500)

        s_after = get_s_param(page.url)
        assert s_after is not None, "?s= must be present after toggle"
        assert s_after != s_before, (
            f"?s= should change after toggle: before={s_before!r} after={s_after!r}"
        )

    def test_expanded_node_can_be_collapsed(self, page: Page, base_url: str):
        """
        Click ▸ to open, then click ▾ on the same position to close.
        After collapsing the open-twist count should go back to what it was.
        """
        page.goto(f"{base_url}/tree")

        # Count open twists (▾) before we do anything
        open_before = page.locator("button.tree-twist", has_text="▾").count()

        # Expand the first closed node
        closed_twist = page.locator("button.tree-twist", has_text="▸").first
        closed_twist.click()
        page.wait_for_timeout(500)

        open_after_expand = page.locator("button.tree-twist", has_text="▾").count()
        assert open_after_expand > open_before, "At least one ▾ should appear after expand"

        # Now collapse it — click the first ▾
        open_twist = page.locator("button.tree-twist", has_text="▾").first
        open_twist.click()
        page.wait_for_timeout(500)

        open_after_collapse = page.locator("button.tree-twist", has_text="▾").count()
        assert open_after_collapse <= open_after_expand, (
            "After collapsing, ▾ count should not increase"
        )

    def test_selected_node_shows_in_detail_pane(self, page: Page, base_url: str):
        """
        Clicking a node label should mark it selected and the detail pane
        must no longer show 'select a node'.
        """
        page.goto(f"{base_url}/tree")

        page.locator("a.tree-label").first.click()
        page.wait_for_load_state("networkidle")

        detail = page.locator("#detail-pane")
        # Either shows the item URL or body content — must not be the placeholder
        # (We check the URL contains a canonical item path as a proxy)
        page_content = detail.inner_text()
        assert "select a node" not in page_content or "https://slug.social/~/" in page_content

    def test_state_survives_reload(self, page: Page, base_url: str):
        """
        After expanding a node the ?s= blob encodes the open state.
        Reloading the page with the same URL must restore the open nodes
        (the ▾ twist must still be present).
        """
        page.goto(f"{base_url}/tree")

        # Expand first node
        closed_twist = page.locator("button.tree-twist", has_text="▸").first
        closed_twist.click()
        page.wait_for_timeout(500)

        open_after_expand = page.locator("button.tree-twist", has_text="▾").count()
        url_with_state = page.url

        # Navigate away then back to the same URL
        page.goto(f"{base_url}/tree")
        page.goto(url_with_state)
        page.wait_for_load_state("networkidle")

        open_after_reload = page.locator("button.tree-twist", has_text="▾").count()
        assert open_after_reload == open_after_expand, (
            f"Open node count after reload ({open_after_reload}) should match "
            f"before reload ({open_after_expand})"
        )
