//! 模擬の決済サービス。HTTP + JSON で決済（charge）を受け、カードのトークンで結果を決める。
//!
//! - `POST /charges` `{"idempotency_key", "amount", "currency", "card_token"}`
//!   - 承認なら 201 `{"id", "status": "succeeded", ...}`、拒否なら 402 `{"status": "declined", "reason"}`。
//!   - 同じ冪等キーには、同じ結果を返す（中身の違う依頼なら 409）。
//! - `GET /charges/{id}` 決済を返す。
//! - `GET /health`
//!
//! カードのトークン:
//! - `tok_visa`: いつも承認する。
//! - `tok_declined`: いつも拒否する。
//! - `tok_limited`: 利用枠 10,000（最小単位）まで承認する（判定は Verus で証明した `authorize`）。
//! - それ以外: 拒否する（unknown card）。
//!
//! 状態はメモリに置く（再起動で消える）。compose の中では backend-proxy と同じネットワークにだけ置き、
//! backend-rust からは届かない。
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

pub const LIMITED_CARD_LIMIT: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ChargeRequest {
    pub idempotency_key: String,
    pub amount: u64,
    pub currency: String,
    pub card_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Succeeded,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Charge {
    pub id: String,
    pub status: Status,
    pub amount: u64,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Default)]
struct Store {
    /// 冪等キー → (依頼, 結果)
    by_key: HashMap<String, (ChargeRequest, Charge)>,
    by_id: HashMap<String, Charge>,
    /// tok_limited で使った額
    spent: u64,
    next: u64,
}

#[derive(Clone, Default)]
pub struct App {
    store: Arc<Mutex<Store>>,
}

fn status_of(c: &Charge) -> StatusCode {
    match c.status {
        Status::Succeeded => StatusCode::CREATED,
        Status::Declined => StatusCode::PAYMENT_REQUIRED,
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn valid_currency(c: &str) -> bool {
    c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase())
}

impl App {
    pub fn charge(&self, req: ChargeRequest) -> Result<Charge, (StatusCode, &'static str)> {
        if req.idempotency_key.is_empty() || req.idempotency_key.len() > 128 {
            return Err((StatusCode::BAD_REQUEST, "idempotency_key must be 1..=128 bytes"));
        }
        if !valid_currency(&req.currency) {
            return Err((StatusCode::BAD_REQUEST, "currency must be three upper-case letters"));
        }
        let mut s = self.store.lock().expect("store lock");
        if let Some((prev, charge)) = s.by_key.get(&req.idempotency_key) {
            return if *prev == req {
                Ok(charge.clone())
            } else {
                Err((StatusCode::CONFLICT, "the idempotency_key was used for a different charge"))
            };
        }
        let decline = |reason: &str| (Status::Declined, Some(reason.to_owned()));
        let (status, reason) = match req.card_token.as_str() {
            _ if req.amount == 0 => decline("amount must be positive"),
            "tok_visa" => (Status::Succeeded, None),
            "tok_declined" => decline("card declined"),
            "tok_limited" => match payment_verified::authorize(s.spent, req.amount, LIMITED_CARD_LIMIT) {
                Some(n) => {
                    s.spent = n;
                    (Status::Succeeded, None)
                }
                None => decline("over the card limit"),
            },
            _ => decline("unknown card"),
        };
        s.next += 1;
        let charge = Charge {
            id: format!("ch_{:08}", s.next),
            status,
            amount: req.amount,
            currency: req.currency.clone(),
            reason,
        };
        s.by_id.insert(charge.id.clone(), charge.clone());
        s.by_key.insert(req.idempotency_key.clone(), (req, charge.clone()));
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
    fn bad_requests_are_rejected() {
        let app = App::default();
        let mut bad = req("y", 10, "tok_visa");
        bad.currency = "yen".to_owned();
        assert_eq!(app.charge(bad).unwrap_err().0, StatusCode::BAD_REQUEST);
    }
}
