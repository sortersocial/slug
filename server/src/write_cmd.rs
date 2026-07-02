use tokio::sync::oneshot;

use slug_types::RpcResult;

pub type WriteCmdResult = Result<RpcResult, (String, Option<String>)>;

pub enum WriteCmd {
    Post {
        room: String,
        thread_tag: String,
        delegate_opt: Option<String>,
        text: String,
        return_rank_diff: bool,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    SystemIngest {
        room: String,
        thread_tag: String,
        text: String,
        principal: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    Redact {
        post_id: String,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    RoomCreate {
        slug: String,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    RoomDelete {
        room: String,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    ThreadGraduate {
        room: String,
        thread_tag: String,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    Grant {
        room: String,
        username: String,
        capabilities: Vec<String>,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    Revoke {
        room: String,
        username: String,
        capability: String,
        bearer: String,
        reply: oneshot::Sender<WriteCmdResult>,
    },
    Register {
        username: String,
        provider: String,
        provider_id: String,
        redeem_invite: Option<String>,
        reply: oneshot::Sender<Result<String, String>>,
    },
    OAuthTokenIssue {
        token_event: crate::events::Event,
        redeem_invite: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RedeemInvite {
        token: String,
        grantee_username: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}
