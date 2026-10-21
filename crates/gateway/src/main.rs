use axum::{Router, routing::get};

#[tokio::main]
async fn main() {
    let addr = std::env::var("GATEWAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let app = Router::new().route("/health", get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("gateway listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}
