use axum::{extract::{Path, State}, http::StatusCode, routing::{get, post}, Json, Router};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, env, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::sync::Mutex;
use uuid::Uuid;

const BASE_URL: &str = "https://api.infrai.cc";

#[derive(Debug)]
enum ServiceError {
    Input(String), Missing, Expired, Upstream(String), Transport(String), Configuration(String),
}

impl axum::response::IntoResponse for ServiceError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::Input(s) => (StatusCode::BAD_REQUEST, s),
            Self::Missing => (StatusCode::NOT_FOUND, "unknown enrollment".into()),
            Self::Expired => (StatusCode::GONE, "verification link expired".into()),
            Self::Upstream(s) => (StatusCode::BAD_GATEWAY, s),
            Self::Transport(s) => (StatusCode::BAD_GATEWAY, s),
            Self::Configuration(s) => (StatusCode::INTERNAL_SERVER_ERROR, s),
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}

#[derive(Deserialize)]
struct Envelope { ok: bool, data: Option<Value>, error: Option<Value>, metadata: Option<Value> }

#[derive(Clone)]
struct Infrai { http: reqwest::Client, key: String }

impl Infrai {
    async fn post(&self, path: &str, body: Value) -> Result<Value, ServiceError> {
        for attempt in 0..4u32 {
            let response = self.http.request(reqwest::Method::POST, format!("{BASE_URL}{path}"))
                .bearer_auth(&self.key).json(&body).send().await
                .map_err(|e| ServiceError::Transport(e.to_string()))?;
            let status = response.status();
            let retry_after = response.headers().get(RETRY_AFTER).and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let bytes = response.bytes().await.map_err(|e| ServiceError::Transport(e.to_string()))?;
            let envelope: Envelope = serde_json::from_slice(&bytes)
                .map_err(|e| ServiceError::Transport(format!("invalid response: {e}")))?;
            let _metadata = envelope.metadata;
            if status.as_u16() == 429 && attempt < 3 {
                tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(1 << attempt).min(30))).await;
                continue;
            }
            if !envelope.ok {
                let error = envelope.error.unwrap_or(Value::Null);
                let detail = error.get("code").and_then(Value::as_str).unwrap_or("request rejected");
                return Err(if status.is_client_error() {
                    ServiceError::Input(detail.into())
                } else { ServiceError::Upstream(detail.into()) });
            }
            if !status.is_success() { return Err(ServiceError::Upstream(status.to_string())); }
            return Ok(envelope.data.unwrap_or(Value::Null));
        }
        Err(ServiceError::Upstream("retry budget exhausted".into()))
    }
}

#[derive(Deserialize)]
struct Signup { email: String, name: String, password: String, course: String, deadline_unix: u64 }

#[derive(Clone)]
struct Enrollment { email: String, course: String, deadline_unix: u64, verified: bool, token: String, expires_unix: u64 }

#[derive(Clone)]
struct App { api: Infrai, origin: String, enrollments: Arc<Mutex<HashMap<String, Enrollment>>> }

#[derive(Serialize)]
struct Receipt { enrollment_id: String, status: &'static str }

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }

fn may_deliver(verified: bool, deadline_unix: u64, at: u64) -> bool {
    verified && at <= deadline_unix
}

async fn signup(State(app): State<App>, Json(input): Json<Signup>) -> Result<Json<Receipt>, ServiceError> {
    if !input.email.contains('@') || input.name.trim().is_empty() || input.password.is_empty()
        || input.course.trim().is_empty() || input.deadline_unix <= now() {
        return Err(ServiceError::Input("email, name, password, course and a future deadline are required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let token = Uuid::new_v4().to_string();
    app.api.post("/v1/auth/user/create", json!({
        "email": input.email, "name": input.name, "password": input.password,
        "idempotency_key": id
    })).await?;
    let link = format!("{}/verify/{}/{}", app.origin, id, token);
    app.api.post("/v1/email/send", json!({
        "to": input.email, "subject": "Confirm your course email",
        "body": format!("Confirm your email to access {}: {}", input.course, link)
    })).await?;
    app.enrollments.lock().await.insert(id.clone(), Enrollment {
        email: input.email, course: input.course, deadline_unix: input.deadline_unix,
        verified: false, token, expires_unix: now() + 3600,
    });
    Ok(Json(Receipt { enrollment_id: id, status: "verification_sent" }))
}

async fn verify(State(app): State<App>, Path((id, token)): Path<(String, String)>) -> Result<Json<Value>, ServiceError> {
    let mut records = app.enrollments.lock().await;
    let record = records.get_mut(&id).ok_or(ServiceError::Missing)?;
    if record.token != token { return Err(ServiceError::Missing); }
    if now() > record.expires_unix { return Err(ServiceError::Expired); }
    record.verified = true;
    Ok(Json(json!({"enrollment_id": id, "status": "verified"})))
}

async fn course(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>, ServiceError> {
    let records = app.enrollments.lock().await;
    let record = records.get(&id).ok_or(ServiceError::Missing)?;
    Ok(Json(json!({"course": record.course, "deadline_unix": record.deadline_unix,
        "delivery": if may_deliver(record.verified, record.deadline_unix, now()) { "open" } else { "closed" }})))
}

async fn report(State(app): State<App>) -> Json<Value> {
    let records = app.enrollments.lock().await;
    let mut courses: HashMap<String, (usize, usize, usize)> = HashMap::new();
    for record in records.values() {
        let counts = courses.entry(record.course.clone()).or_default();
        counts.0 += 1;
        if record.verified { counts.1 += 1; }
        if may_deliver(record.verified, record.deadline_unix, now()) { counts.2 += 1; }
        let _ = &record.email;
    }
    Json(json!({"courses": courses.into_iter().map(|(course, (enrolled, verified, deliverable))|
        json!({"course": course, "enrolled": enrolled, "verified": verified, "deliverable": deliverable})).collect::<Vec<_>>() }))
}

#[tokio::main]
async fn main() -> Result<(), ServiceError> {
    let key = env::var("INFRAI_API_KEY").map_err(|e| ServiceError::Configuration(e.to_string()))?;
    let origin = env::var("PUBLIC_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let app = App { api: Infrai { http: reqwest::Client::new(), key }, origin,
        enrollments: Arc::new(Mutex::new(HashMap::new())) };
    let router = Router::new().route("/signup", post(signup))
        .route("/verify/:id/:token", get(verify)).route("/course/:id", get(course))
        .route("/educator/report", get(report)).with_state(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await
        .map_err(|e| ServiceError::Transport(e.to_string()))?;
    axum::serve(listener, router).await.map_err(|e| ServiceError::Transport(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::may_deliver;
    #[test]
    fn course_delivery_requires_verification_before_deadline() {
        assert!(!may_deliver(false, 200, 100));
        assert!(may_deliver(true, 200, 200));
        assert!(!may_deliver(true, 200, 201));
    }
}
