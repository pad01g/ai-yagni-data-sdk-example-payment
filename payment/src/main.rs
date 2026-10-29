//! 模擬の決済サービスを起動する。`LISTEN`（既定 `0.0.0.0:8080`）で待つ。
#[tokio::main]
async fn main() {
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener =
        tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| panic!("cannot listen on {addr}: {e}"));
    eprintln!("payment: listening on {addr}");
    axum::serve(listener, payment::router(payment::App::default()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}
