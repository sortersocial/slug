# Muse Connector Platform for slug

Research note (2026-09-20). How Meta's consumer Muse agent connects to third-party products, and how slug.social maps onto that surface the same way it already maps onto ChatGPT / Codex MCP.

This is **not** Muse Code (the terminal coding agent on `dev.meta.ai`) and **not** Meta Model API (`https://api.meta.ai/v1`). Those products already speak MCP or OpenAI/Anthropic-compatible inference. Consumer Muse (muse.ai) is a personal agent on a Meta VM. It has no "add MCP server" setting.

Canonical pages:

- [Muse Connector Platform](https://muse.ai/platform)
- [How Muse works with Connectors](https://www.meta.com/help/artificial-intelligence/1687253048996149/)
- [Muse Code MCP (different product)](https://dev.meta.ai/docs/muse-code/extending/)
- Existing slug MCP note: `ideas/chatgpt-mcp-app.md`

---

## What Meta published

On 18 September 2026 Meta opened a developer door at `muse.ai/platform`: describe the product, submit for functional / security / legal review plus Meta end-to-end testing, appear in the Connectors directory. Stripe Link is the payments rail. As of 20 September 2026 that page still has **no SDK, no OpenAPI template, no OAuth callback list, and no developer terms**.

Two classes already exist in the help center:

| Class | Who builds it | Review | How it is invoked |
| --- | --- | --- | --- |
| Directory connector | Business submits at muse.ai/platform | Meta | Settings → Connectors, one tap |
| Custom connector | Muse writes client code from a public API spec | None | User asks Muse to "build a custom connector" |

Custom connectors are the working path **today**. Muse fetches a public spec from its VM, writes client code, stores a bearer in the Secure Credentials Store, and Sentinel swaps a surrogate token at the network boundary. Parallel / AdaptlyPost (checked 20 September 2026) confirmed: no MCP setting, REST + OpenAPI 3.0.3, `Authorization: Bearer`, first calls are a liveness/status check then a credential check.

Muse Code remains the MCP client: `mcp_servers` with `transport: streamable_http` pointed at `https://slug.social/mcp`. Do not conflate the two products.

---

## Why slug cannot reuse `POST /mcp` as the Muse connector

Same reasons ChatGPT widgets cannot reuse `POST /ui` + `eval`:

1. Consumer Muse does not speak JSON-RPC MCP.
2. The agent VM cannot reach localhost / stdio MCP.
3. Custom-connector practice is a **public, unauthenticated OpenAPI document** plus a bearer pasted into a credential prompt, never into chat.
4. OAuth-only remote MCP adds friction in that flow; `slug_…` already is a bearer.

The MCP server stays the ChatGPT / Claude / Muse Code surface. Muse consumer gets a **second wire format, same verbs**.

```
Muse (consumer)
    │  GET  /muse/v1/openapi.json   (no auth)
    │  GET  /muse/v1/status         (no auth)
    │  GET  /muse/v1/whoami         (Bearer slug_…)
    │  POST /muse/v1/<tool>         (JSON = MCP inputSchema)
    ▼
slug Muse REST  (server/src/muse/)
    │  call_named_tool
    ▼
same dispatch_rpc / events.jsonl as POST /mcp and POST /api/v0/rpc
```

---

## Mapping (v1)

Same catalog as MCP `tools/list`. No new `RpcCommand`.

| Muse path | MCP tool | Auth |
| --- | --- | --- |
| `GET /muse/v1/status` | `health` | none |
| `GET /muse/v1/whoami` | `whoami` | bearer |
| `GET /muse/v1/openapi.json` | (spec) | none |
| `GET /muse/v1/docs.md` | (setup brief) | none |
| `POST /muse/v1/<name>` | that tool | same as MCP `securitySchemes` |

`GET /status` is the Muse-facing STAT: unauthenticated `{ok:true,status:ok}`, same payload as MCP `health` and `GET /healthz`. Call it first so Muse can prove the spec host is live before it stores a credential.

Write rules stay the MCP rules: `post_sorter` requires `delegate`; server binds it to the linked human; `create_room` is private-only. Muse should mint `rig=muse` `model=meta/muse` via `identity_start`.

OAuth 2.1 at `/oauth/authorize` allowlists `muse.ai` / `meta.ai` (plus the existing ChatGPT / Claude / loopback hosts) for a future directory connector that does PKCE instead of a pasted key. Directory submission itself is still a form at muse.ai/platform pointing at `https://slug.social/muse/v1/openapi.json`.

---

## What not to do

- Do not add a parallel write path.
- Do not reuse `POST /ui` + `eval` morph as a Muse widget.
- Do not ask Muse to connect `POST /mcp` until Meta ships an MCP setting for the consumer agent.
- Do not put `slug_` tokens in tool results, docs examples, or chat paste templates.
- Do not expose GitHub / Are.na resolvers as Muse tools (same unofficial-connector risk as the ChatGPT listing).

---

## Implementation status (this branch)

v1 is in `server/src/muse/`:

- `GET /muse/v1` — connector index (status / spec / docs / mcp URLs)
- `GET /muse/v1/status` — STAT liveness
- `GET /muse/v1/whoami` — bearer credential check
- `GET /muse/v1/openapi.json` — generated from MCP `tools_list`
- `GET /muse/v1/docs.md` — public setup brief Muse can fetch
- `POST /muse/v1/:tool` — `call_named_tool` (same dispatch as MCP)

Still later: fill the muse.ai/platform directory form, reviewer account, optional Stripe Link (slug has no checkout).
