//! 模擬の決済サービス。HTTP + JSON で決済（charge）を受け、カードのトークンで結果を決める。
//!
//! - `POST /charges` `{"idempotency_key", "amount", "currency", "card_token"}`
//!   - 承認なら 201 `{"id", "status": "succeeded", ...}`、拒否なら 402 `{"status": "declined", "reason"}`。
//!   - 同じ冪等キーには、同じ結果を返す（中身の違う依頼なら 409）。
//!   - 依頼の形が正しくなければ 400（拒否とは別。記録もしない）。
//! - `GET /charges/{id}` 決済を返す。
//! - `GET /health`
//!
//! カードのトークン:
//! - `tok_visa`: いつも承認する。
//! - `tok_declined`: いつも拒否する。
//! - `tok_limited`: 累計の利用枠 10,000（最小単位）まで承認する（判定は Verus で証明した `authorize`）。
//! - それ以外: 拒否する（unknown card）。
//!
//! 本物の決済サービスと同じく、冪等キーと決済は再起動をまたいで覚えている。決済は 1 件ずつ
//! 追記だけのファイル（[`Journal`]、1 行に 1 件の JSON）に書き、起動したときに読み直す。
//! 決済 ID は乱数で作る（再起動しても前の ID と重ならない）。
//!
//! compose の中では backend-proxy と同じネットワークにだけ置き、backend-rust からは届かない。
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub const LIMITED_CARD_LIMIT: u64 = 10_000;
/// 覚えておく決済の数の上限（模擬のサービスがメモリを使い切らないように）。
pub const MAX_CHARGES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChargeRequest {
    pub idempotency_key: String,
    pub amount: u64,
    pub currency: String,
    pub card_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Charge {
    pub id: String,
    pub status: Outcome,
    pub amount: u64,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 依頼の誤り（HTTP の状態と、利用者向けではない説明）。
pub type Rejection = (StatusCode, &'static str);

/// 決済の記録。1 行に `{"request": .., "charge": ..}` を 1 つ書く。
pub struct Journal(File);

#[derive(Serialize, Deserialize)]
struct Entry {
    request: ChargeRequest,
    charge: Charge,
}

impl Journal {
    /// ファイルを開き、そこまでの記録を返す（なければ作る）。
    pub fn open(path: &FsPath) -> std::io::Result<(Self, Vec<(ChargeRequest, Charge)>)> {
        let file = OpenOptions::new().create(true).read(true).append(true).open(path)?;
        let mut entries = Vec::new();
        for line in BufReader::new(&file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let e: Entry = serde_json::from_str(&line).map_err(std::io::Error::other)?;
            entries.push((e.request, e.charge));
        }
        Ok((Self(file), entries))
    }

    fn append(&mut self, request: &ChargeRequest, charge: &Charge) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(&Entry { request: request.clone(), charge: charge.clone() })?;
        line.push(b'\n');
        self.0.write_all(&line)?;
        self.0.sync_data()
    }
}

#[derive(Default)]
struct Store {
    /// 冪等キー → (依頼, 決済 ID)
    by_key: HashMap<String, (ChargeRequest, String)>,
    by_id: HashMap<String, Charge>,
    /// tok_limited で使った額
    spent: u64,
    journal: Option<Journal>,
}

impl Store {
    fn remember(&mut self, req: ChargeRequest, charge: Charge) {
        if req.card_token == "tok_limited" && charge.status == Outcome::Succeeded {
            self.spent = self.spent.saturating_add(charge.amount);
        }
        self.by_key.insert(req.idempotency_key.clone(), (req, charge.id.clone()));
        self.by_id.insert(charge.id.clone(), charge);
    }
}

#[derive(Clone, Default)]
pub struct App {
    store: Arc<Mutex<Store>>,
}

fn status_of(c: &Charge) -> StatusCode {
    match c.status {
        Outcome::Succeeded => StatusCode::CREATED,
        Outcome::Declined => StatusCode::PAYMENT_REQUIRED,
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn valid_currency(c: &str) -> bool {
    c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase())
}

fn validate(req: &ChargeRequest) -> Result<(), Rejection> {
    if req.idempotency_key.is_empty() || req.idempotency_key.len() > 128 {
        return Err((StatusCode::BAD_REQUEST, "idempotency_key must be 1..=128 bytes"));
    }
    if req.card_token.is_empty() || req.card_token.len() > 64 {
        return Err((StatusCode::BAD_REQUEST, "card_token must be 1..=64 bytes"));
    }
    if !valid_currency(&req.currency) {
        return Err((StatusCode::BAD_REQUEST, "currency must be three upper-case letters"));
    }
    if req.amount == 0 {
        return Err((StatusCode::BAD_REQUEST, "amount must be positive"));
    }
    Ok(())
}

/// 推測できない決済 ID。
fn new_id() -> String {
    let mut b = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("ch_{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

impl App {
    /// 記録のファイルを読み直したサービス。
    pub fn with_journal(path: &FsPath) -> std::io::Result<Self> {
        let (journal, entries) = Journal::open(path)?;
        let mut store = Store::default();
        for (req, charge) in entries {
            store.remember(req, charge);
        }
        store.journal = Some(journal);
        Ok(Self { store: Arc::new(Mutex::new(store)) })
    }

    pub fn charge(&self, req: ChargeRequest) -> Result<Charge, Rejection> {
        validate(&req)?;
        let mut s = self.store.lock().expect("store lock");
        if let Some((prev, id)) = s.by_key.get(&req.idempotency_key) {
            return if *prev == req {
                Ok(s.by_id[id].clone())
            } else {
                Err((StatusCode::CONFLICT, "the idempotency_key was used for a different charge"))
            };
        }
        if s.by_key.len() >= MAX_CHARGES {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "too many charges"));
        }
        let decline = |reason: &str| (Outcome::Declined, Some(reason.to_owned()));
        let (status, reason) = match req.card_token.as_str() {
            "tok_visa" => (Outcome::Succeeded, None),
            "tok_declined" => decline("card declined"),
            "tok_limited" => match payment_verified::authorize(s.spent, req.amount, LIMITED_CARD_LIMIT) {
                Some(_) => (Outcome::Succeeded, None),
                None => decline("over the card limit"),
            },
            _ => decline("unknown card"),
        };
        let charge = Charge { id: new_id(), status, amount: req.amount, currency: req.currency.clone(), reason };
        // 記録できなければ決済しない（再起動の後に同じキーで二重に決済しないため）
        if let Some(j) = s.journal.as_mut()
            && let Err(e) = j.append(&req, &charge)
        {
            eprintln!("payment: cannot write the journal: {e}");
            return Err((StatusCode::SERVICE_UNAVAILABLE, "cannot record the charge"));
        }
        s.remember(req, charge.clone());
        Ok(charge)
    }

    pub fn find(&self, id: &str) -> Option<Charge> {
        self.store.lock().expect("store lock").by_id.get(id).cloned()
    }
}

async fn create(State(app): State<App>, Json(req): Json<ChargeRequest>) -> Response {
    match app.charge(req) {
        Ok(c) => (status_of(&c), Json(c)).into_response(),
        Err((status, message)) => error(status, message),
    }
}

async fn show(State(app): State<App>, Path(id): Path<String>) -> Response {
    match app.find(&id) {
        Some(c) => Json(c).into_response(),
        None => error(StatusCode::NOT_FOUND, "no such charge"),
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/charges", post(create))
        .route("/charges/{id}", get(show))
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn req(key: &str, amount: u64, card: &str) -> ChargeRequest {
        ChargeRequest {
            idempotency_key: key.to_owned(),
            amount,
            currency: "JPY".to_owned(),
            card_token: card.to_owned(),
        }
    }

    async fn post(router: Router, body: &ChargeRequest) -> (StatusCode, serde_json::Value) {
        let r = router
            .oneshot(
                Request::post("/charges")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status();
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn a_good_card_is_charged_once_per_key() {
        let app = App::default();
        let (s1, b1) = post(router(app.clone()), &req("k1", 2500, "tok_visa")).await;
        let (s2, b2) = post(router(app.clone()), &req("k1", 2500, "tok_visa")).await;
        assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
        assert_eq!(b1, b2);
        assert_eq!(b1["status"], "succeeded");
        let (s3, _) = post(router(app), &req("k1", 9999, "tok_visa")).await;
        assert_eq!(s3, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn declined_cards_and_limits() {
        let app = App::default();
        let (s, b) = post(router(app.clone()), &req("d", 100, "tok_declined")).await;
        assert_eq!((s, b["reason"].as_str()), (StatusCode::PAYMENT_REQUIRED, Some("card declined")));
        assert_eq!(post(router(app.clone()), &req("l1", 6000, "tok_limited")).await.0, StatusCode::CREATED);
        let (s, b) = post(router(app.clone()), &req("l2", 5000, "tok_limited")).await;
        assert_eq!((s, b["reason"].as_str()), (StatusCode::PAYMENT_REQUIRED, Some("over the card limit")));
        assert_eq!(post(router(app), &req("l3", 4000, "tok_limited")).await.0, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn charges_can_be_looked_up() {
        let app = App::default();
        let c = app.charge(req("x", 10, "tok_visa")).unwrap();
        let r =
            router(app).oneshot(Request::get(format!("/charges/{}", c.id)).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[test]
    fn bad_requests_are_rejected_and_not_recorded() {
        let app = App::default();
        let mut bad = req("y", 10, "tok_visa");
        bad.currency = "yen".to_owned();
        assert_eq!(app.charge(bad).unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(app.charge(req("z", 0, "tok_visa")).unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(app.charge(req("w", 10, &"t".repeat(65))).unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(app.charge(req("", 10, "tok_visa")).unwrap_err().0, StatusCode::BAD_REQUEST);
        // 正しい形で出し直せる（誤りは記録されていない）
        assert_eq!(app.charge(req("z", 10, "tok_visa")).unwrap().status, Outcome::Succeeded);
    }

    #[test]
    fn ids_are_unpredictable() {
        let app = App::default();
        let a = app.charge(req("a", 10, "tok_visa")).unwrap().id;
        let b = app.charge(req("b", 10, "tok_visa")).unwrap().id;
        assert_ne!(a, b);
        assert_eq!(a.len(), 3 + 24);
    }

    #[test]
    fn charges_and_limits_survive_a_restart() {
        let path = std::env::temp_dir().join(format!("payment-journal-{}", new_id()));
        let first = App::with_journal(&path).unwrap();
        let charged = first.charge(req("k", 6000, "tok_limited")).unwrap();
        drop(first);
        let second = App::with_journal(&path).unwrap();
        // 同じ依頼には同じ決済（二重に決済しない）
        assert_eq!(second.charge(req("k", 6000, "tok_limited")).unwrap(), charged);
        // 使った額も覚えている
        assert_eq!(second.charge(req("k2", 5000, "tok_limited")).unwrap().status, Outcome::Declined);
        assert_eq!(second.find(&charged.id), Some(charged));
        std::fs::remove_file(path).unwrap();
    }
}
