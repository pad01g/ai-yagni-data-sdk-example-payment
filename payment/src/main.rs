//! 模擬の決済サービスを起動する。
//!
//! - `LISTEN`: 待つ場所（既定 `0.0.0.0:8080`）。
//! - `JOURNAL`: 決済の記録のファイル。なければメモリにだけ置く（再起動で消える。テストや手元の試し用）。
use payment::App;

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let app = match std::env::var("JOURNAL") {
        Ok(path) => App::with_journal(path.as_ref()).unwrap_or_else(|e| panic!("cannot open the journal {path}: {e}")),
        Err(_) => {
            eprintln!("payment: JOURNAL is not set; charges are kept in memory only");
            App::default()
        }
    };
    let listener =
        tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| panic!("cannot listen on {addr}: {e}"));
    eprintln!("payment: listening on {addr}");
    axum::serve(listener, payment::router(app)).with_graceful_shutdown(shutdown()).await.expect("server");
}
