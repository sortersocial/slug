use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};

pub async fn view_count_middleware(
    req: Request,
    next: Next,
) -> Response {
    next.run(req).await
}
