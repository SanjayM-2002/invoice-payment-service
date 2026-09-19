use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

tokio::task_local! {
    pub static REQUEST_ID: String;
}

pub fn current() -> String {
    REQUEST_ID.try_with(|id| id.clone()).unwrap_or_default()
}

pub async fn middleware(req: Request, next: Next) -> Response {
    let id = format!("req_{}", Uuid::now_v7().simple());

    let mut res = REQUEST_ID.scope(id.clone(), next.run(req)).await;

    if let Ok(value) = HeaderValue::from_str(&id) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
    res
}
