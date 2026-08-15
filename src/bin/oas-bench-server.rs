use bytes::Bytes;
use oas_rs::{ApiError, App, Header, HeaderSpec, Json, JsonBytes, Params, Path, Query, State};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use tokio_postgres::{Client, NoTls, Statement};

#[derive(Deserialize, oas_rs::OpenApi)]
struct Search {
    page: u32,
    active: bool,
}

#[derive(Clone, Debug)]
struct TraceId(String);

impl HeaderSpec for TraceId {
    const NAME: &'static str = "x-trace-id";

    fn parse(value: &str) -> Result<Self, oas_rs::ApiError> {
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug)]
struct ApiKey(String);

impl HeaderSpec for ApiKey {
    const NAME: &'static str = "x-api-key";

    fn parse(value: &str) -> Result<Self, oas_rs::ApiError> {
        if value == "abc-secret" {
            Ok(Self(value.to_owned()))
        } else {
            Err(ApiError::new(
                http::StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "invalid API key",
            ))
        }
    }
}

#[derive(Clone, Default)]
struct BenchState {
    db: Option<Arc<DbPool>>,
}

struct DbConnection {
    client: Client,
    statement: Statement,
}

struct DbPool {
    connections: Vec<DbConnection>,
    next: AtomicUsize,
}

#[derive(Serialize, oas_rs::OpenApi)]
struct DbUser {
    id: i64,
    name: String,
    email: String,
    active: bool,
}

impl DbPool {
    async fn connect(url: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut connections = Vec::with_capacity(16);
        for _ in 0..16 {
            let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
            tokio::spawn(async move {
                if let Err(error) = connection.await {
                    eprintln!("postgres connection error: {error}");
                }
            });
            let statement = client
                .prepare("SELECT id, name, email, active FROM users ORDER BY id LIMIT 100")
                .await?;
            connections.push(DbConnection { client, statement });
        }
        Ok(Self {
            connections,
            next: AtomicUsize::new(0),
        })
    }

    async fn users(&self) -> Result<Vec<DbUser>, tokio_postgres::Error> {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.connections.len();
        let connection = &self.connections[index];
        let rows = connection.client.query(&connection.statement, &[]).await?;
        Ok(rows
            .into_iter()
            .map(|row| DbUser {
                id: row.get(0),
                name: row.get(1),
                email: row.get(2),
                active: row.get(3),
            })
            .collect())
    }
}

async fn path_integer(Path(id): Path<u64>) -> String {
    id.to_string()
}

async fn path_uuid(Path(id): Path<uuid::Uuid>) -> String {
    id.to_string()
}

async fn query_typed(Query(query): Query<Search>) -> String {
    format!("{}:{}", query.page, query.active)
}

async fn params_typed(params: Params) -> String {
    format!(
        "{}:{}",
        params.get("org").expect("org capture"),
        params.get("user").expect("user capture")
    )
}

async fn header_typed(Header(header): Header<TraceId>) -> String {
    header.0
}

async fn validation_success(Path(id): Path<u64>) -> Result<Json<BenchPayload>, ApiError> {
    if id == 42 {
        Ok(Json(BenchPayload {
            name: "valid-42".to_owned(),
        }))
    } else {
        Err(ApiError::bad_request("id must be 42"))
    }
}

async fn problem() -> Result<Json<BenchPayload>, ApiError> {
    Err(ApiError::bad_request("invalid request"))
}

async fn secure(Header(key): Header<ApiKey>) -> String {
    if key.0 == "abc-secret" {
        "authorized".to_owned()
    } else {
        "unauthorized".to_owned()
    }
}

#[derive(Deserialize, Serialize, oas_rs::OpenApi)]
struct ReferenceUser {
    id: u32,
    name: String,
    email: String,
    active: bool,
}

static REFERENCE_USERS: OnceLock<Vec<ReferenceUser>> = OnceLock::new();

fn reference_users() -> &'static Vec<ReferenceUser> {
    REFERENCE_USERS.get_or_init(|| {
        (1..=100)
            .map(|id| ReferenceUser {
                id,
                name: format!("User {id}"),
                email: format!("user{id}@example.com"),
                active: true,
            })
            .collect()
    })
}

async fn json_users() -> Json<&'static Vec<ReferenceUser>> {
    Json(reference_users())
}

async fn users_db(State(state): State<BenchState>) -> Result<Json<Vec<DbUser>>, ApiError> {
    let pool = state.db.as_ref().ok_or_else(|| {
        ApiError::new(
            http::StatusCode::SERVICE_UNAVAILABLE,
            "Database Unavailable",
            "DATABASE_URL is not configured",
        )
    })?;
    pool.users().await.map(Json).map_err(|error| {
        ApiError::new(
            http::StatusCode::BAD_GATEWAY,
            "Database Error",
            error.to_string(),
        )
    })
}

#[derive(Serialize, oas_rs::OpenApi)]
struct BenchPayload {
    name: String,
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let implementation = std::env::var("OAS_IMPLEMENTATION").unwrap_or_else(|_| "oas".to_owned());
    let address = std::env::var("OAS_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let db = match std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty())
    {
        Some(url) => Some(Arc::new(DbPool::connect(&url).await?)),
        None => None,
    };
    if implementation == "raw" {
        return raw_server(&address, db).await;
    }
    let static_json = JsonBytes::new(Bytes::from_static(br#"{"id":1,"name":"Alice"}"#));
    let small_json = static_json.clone();
    let users_static = JsonBytes::new(users_json().clone());
    let mut app = App::new()
        .with_state(BenchState { db })
        .title("oas-rs benchmark")
        .version("0.1.0");
    app.get("/plaintext", || async { "OK" });
    app.get("/json-static", move || {
        let value = static_json.clone();
        async move { value }
    });
    app.get("/fixed/path", || async { "OK" });
    app.get("/users/{id}", path_integer);
    app.get("/uuid/{id}", path_uuid);
    app.get("/search", query_typed);
    app.get("/params/{org}/{user}", params_typed);
    app.get("/trace", header_typed);
    app.get("/validation-success/{id}", validation_success);
    app.get("/problem", problem);
    app.raw_get("/raw-handler", |_request| async { "OK" });
    app.get("/secure", secure);
    app.get("/json-small", move || {
        let value = small_json.clone();
        async move { value }
    });
    app.get("/users-static", move || {
        let value = users_static.clone();
        async move { value }
    });
    app.get("/users", json_users);
    app.get("/users-db", users_db);
    app.openapi("/openapi.json").swagger("/swagger");
    app.listen(&address).await
}

async fn raw_server(
    address: &str,
    db: Option<Arc<DbPool>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use bytes::Bytes;
    use http::{Request, Response, StatusCode};
    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;

    let listener = tokio::net::TcpListener::bind(address).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let db = db.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                let db = db.clone();
                async move {
                    let response = if request.method() != http::Method::GET {
                        Response::builder()
                            .status(StatusCode::METHOD_NOT_ALLOWED)
                            .header("allow", "GET")
                            .body(Full::new(Bytes::new()))
                            .unwrap()
                    } else if request.uri().path() == "/plaintext"
                        || request.uri().path() == "/fixed/path"
                    {
                        Response::builder()
                            .status(StatusCode::OK)
                            .body(Full::new(Bytes::from_static(b"OK")))
                            .unwrap()
                    } else if request.uri().path() == "/json-static"
                        || request.uri().path() == "/json-small"
                    {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from_static(br#"{"id":1,"name":"Alice"}"#)))
                            .unwrap()
                    } else if request.uri().path() == "/users-static" {
                        let body = users_json().clone();
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body))
                            .unwrap()
                    } else if request.uri().path() == "/users-db" {
                        match query_db_bytes(db.clone()).await {
                            Ok(body) => Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "application/json")
                                .body(Full::new(body))
                                .unwrap(),
                            Err(status) => Response::builder()
                                .status(status)
                                .body(Full::new(Bytes::from_static(b"database error")))
                                .unwrap(),
                        }
                    } else if request.uri().path() == "/users" {
                        let body = Bytes::from(serde_json::to_vec(reference_users()).unwrap());
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(body))
                            .unwrap()
                    } else if request.uri().path() == "/raw-handler" {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "text/plain; charset=utf-8")
                            .header("content-length", "2")
                            .body(Full::new(Bytes::from_static(b"OK")))
                            .unwrap()
                    } else if request.uri().path() == "/problem" {
                        let body = Bytes::from_static(
                            br#"{"type":"about:blank","title":"Bad Request","status":400,"detail":"invalid request"}"#,
                        );
                        Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header("content-type", "application/json")
                            .header("content-length", body.len())
                            .body(Full::new(body))
                            .unwrap()
                    } else if let Some(value) =
                        request.uri().path().strip_prefix("/validation-success/")
                    {
                        match value.parse::<u64>() {
                            Ok(42) => {
                                let body = Bytes::from_static(br#"{"name":"valid-42"}"#);
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "application/json")
                                    .header("content-length", body.len())
                                    .body(Full::new(body))
                                    .unwrap()
                            }
                            _ => Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .body(Full::new(Bytes::from_static(b"Bad Request")))
                                .unwrap(),
                        }
                    } else if request.uri().path() == "/secure" {
                        let authorized = request
                            .headers()
                            .get("x-api-key")
                            .and_then(|value| value.to_str().ok())
                            == Some("abc-secret");
                        let body: &[u8] = if authorized {
                            b"authorized"
                        } else {
                            b"Unauthorized"
                        };
                        Response::builder()
                            .status(if authorized {
                                StatusCode::OK
                            } else {
                                StatusCode::UNAUTHORIZED
                            })
                            .header("content-type", "text/plain; charset=utf-8")
                            .header("content-length", body.len())
                            .body(Full::new(Bytes::copy_from_slice(body)))
                            .unwrap()
                    } else if let Some(value) = request.uri().path().strip_prefix("/params/") {
                        let body = match value.split_once('/') {
                            Some((org, user)) if !org.is_empty() && !user.is_empty() => {
                                Bytes::from(format!("{org}:{user}"))
                            }
                            _ => Bytes::from_static(b"Bad Request"),
                        };
                        Response::builder()
                            .status(if body.as_ref() == b"Bad Request" {
                                StatusCode::BAD_REQUEST
                            } else {
                                StatusCode::OK
                            })
                            .header("content-type", "text/plain; charset=utf-8")
                            .header("content-length", body.len())
                            .body(Full::new(body))
                            .unwrap()
                    } else if let Some(value) = request.uri().path().strip_prefix("/users/") {
                        match value.parse::<u64>() {
                            Ok(id) => Response::builder()
                                .status(StatusCode::OK)
                                .body(Full::new(Bytes::from(id.to_string())))
                                .unwrap(),
                            Err(_) => Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .body(Full::new(Bytes::from_static(b"Bad Request")))
                                .unwrap(),
                        }
                    } else if let Some(value) = request.uri().path().strip_prefix("/uuid/") {
                        match value.parse::<uuid::Uuid>() {
                            Ok(id) => Response::builder()
                                .status(StatusCode::OK)
                                .body(Full::new(Bytes::from(id.to_string())))
                                .unwrap(),
                            Err(_) => Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .body(Full::new(Bytes::from_static(b"Bad Request")))
                                .unwrap(),
                        }
                    } else if request.uri().path() == "/search" {
                        let valid_query = request.uri().query() == Some("page=42&active=true");
                        Response::builder()
                            .status(if valid_query {
                                StatusCode::OK
                            } else {
                                StatusCode::BAD_REQUEST
                            })
                            .body(Full::new(Bytes::from_static(if valid_query {
                                b"42:true"
                            } else {
                                b"Bad Request"
                            })))
                            .unwrap()
                    } else if request.uri().path() == "/trace" {
                        let value = request
                            .headers()
                            .get("x-trace-id")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned);
                        let body = value.unwrap_or_else(|| "Bad Request".to_owned());
                        Response::builder()
                            .status(if body == "Bad Request" {
                                StatusCode::BAD_REQUEST
                            } else {
                                StatusCode::OK
                            })
                            .body(Full::new(Bytes::from(body)))
                            .unwrap()
                    } else {
                        Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Full::new(Bytes::from_static(b"Not Found")))
                            .unwrap()
                    };
                    Ok::<_, std::convert::Infallible>(response)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

async fn query_db_bytes(db: Option<Arc<DbPool>>) -> Result<Bytes, http::StatusCode> {
    let pool = db.ok_or(http::StatusCode::SERVICE_UNAVAILABLE)?;
    let users = pool
        .users()
        .await
        .map_err(|_| http::StatusCode::BAD_GATEWAY)?;
    serde_json::to_vec(&users)
        .map(Bytes::from)
        .map_err(|_| http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
const USERS_INPUT_JSON: &[u8] = br#"[{"id":1,"name":"User 1"},{"id":2,"name":"User 2"},{"id":3,"name":"User 3"},{"id":4,"name":"User 4"},{"id":5,"name":"User 5"},{"id":6,"name":"User 6"},{"id":7,"name":"User 7"},{"id":8,"name":"User 8"},{"id":9,"name":"User 9"},{"id":10,"name":"User 10"},{"id":11,"name":"User 11"},{"id":12,"name":"User 12"},{"id":13,"name":"User 13"},{"id":14,"name":"User 14"},{"id":15,"name":"User 15"},{"id":16,"name":"User 16"},{"id":17,"name":"User 17"},{"id":18,"name":"User 18"},{"id":19,"name":"User 19"},{"id":20,"name":"User 20"},{"id":21,"name":"User 21"},{"id":22,"name":"User 22"},{"id":23,"name":"User 23"},{"id":24,"name":"User 24"},{"id":25,"name":"User 25"},{"id":26,"name":"User 26"},{"id":27,"name":"User 27"},{"id":28,"name":"User 28"},{"id":29,"name":"User 29"},{"id":30,"name":"User 30"},{"id":31,"name":"User 31"},{"id":32,"name":"User 32"},{"id":33,"name":"User 33"},{"id":34,"name":"User 34"},{"id":35,"name":"User 35"},{"id":36,"name":"User 36"},{"id":37,"name":"User 37"},{"id":38,"name":"User 38"},{"id":39,"name":"User 39"},{"id":40,"name":"User 40"},{"id":41,"name":"User 41"},{"id":42,"name":"User 42"},{"id":43,"name":"User 43"},{"id":44,"name":"User 44"},{"id":45,"name":"User 45"},{"id":46,"name":"User 46"},{"id":47,"name":"User 47"},{"id":48,"name":"User 48"},{"id":49,"name":"User 49"},{"id":50,"name":"User 50"},{"id":51,"name":"User 51"},{"id":52,"name":"User 52"},{"id":53,"name":"User 53"},{"id":54,"name":"User 54"},{"id":55,"name":"User 55"},{"id":56,"name":"User 56"},{"id":57,"name":"User 57"},{"id":58,"name":"User 58"},{"id":59,"name":"User 59"},{"id":60,"name":"User 60"},{"id":61,"name":"User 61"},{"id":62,"name":"User 62"},{"id":63,"name":"User 63"},{"id":64,"name":"User 64"},{"id":65,"name":"User 65"},{"id":66,"name":"User 66"},{"id":67,"name":"User 67"},{"id":68,"name":"User 68"},{"id":69,"name":"User 69"},{"id":70,"name":"User 70"},{"id":71,"name":"User 71"},{"id":72,"name":"User 72"},{"id":73,"name":"User 73"},{"id":74,"name":"User 74"},{"id":75,"name":"User 75"},{"id":76,"name":"User 76"},{"id":77,"name":"User 77"},{"id":78,"name":"User 78"},{"id":79,"name":"User 79"},{"id":80,"name":"User 80"},{"id":81,"name":"User 81"},{"id":82,"name":"User 82"},{"id":83,"name":"User 83"},{"id":84,"name":"User 84"},{"id":85,"name":"User 85"},{"id":86,"name":"User 86"},{"id":87,"name":"User 87"},{"id":88,"name":"User 88"},{"id":89,"name":"User 89"},{"id":90,"name":"User 90"},{"id":91,"name":"User 91"},{"id":92,"name":"User 92"},{"id":93,"name":"User 93"},{"id":94,"name":"User 94"},{"id":95,"name":"User 95"},{"id":96,"name":"User 96"},{"id":97,"name":"User 97"},{"id":98,"name":"User 98"},{"id":99,"name":"User 99"},{"id":100,"name":"User 100"}]"#;

#[cfg(test)]
#[derive(Deserialize, Serialize, oas_rs::OpenApi)]
struct BenchUser {
    id: u32,
    name: String,
}

#[cfg(test)]
static SHARED_USERS: OnceLock<Vec<BenchUser>> = OnceLock::new();

#[cfg(test)]
fn shared_users() -> &'static Vec<BenchUser> {
    SHARED_USERS.get_or_init(|| serde_json::from_slice(USERS_INPUT_JSON).unwrap())
}

static USERS_JSON: OnceLock<Bytes> = OnceLock::new();

fn users_json() -> &'static Bytes {
    USERS_JSON.get_or_init(|| Bytes::from(serde_json::to_vec(reference_users()).unwrap()))
}

#[cfg(test)]
fn build_users_json() -> Bytes {
    Bytes::from(serde_json::to_vec(shared_users()).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_users_are_serialized_from_the_shared_dto() {
        let bytes = build_users_json();
        let users: Vec<BenchUser> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(users.len(), 100);
        assert_eq!(users[0].name, "User 1");
        assert_eq!(users[99].name, "User 100");
    }

    #[test]
    fn reference_payload_is_prepared_once_and_has_one_hundred_full_users() {
        let encoded = users_json();
        let users: Vec<ReferenceUser> = serde_json::from_slice(encoded).unwrap();
        assert_eq!(users.len(), 100);
        assert_eq!(users[0].email, "user1@example.com");
        assert!(users[99].active);
        assert_eq!(
            encoded.as_ref(),
            serde_json::to_vec(reference_users()).unwrap()
        );
    }
}
