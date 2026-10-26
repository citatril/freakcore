use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Extension, Form, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Deserialize;
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS members(id INTEGER PRIMARY KEY, name TEXT NOT NULL, pledge_class TEXT, title TEXT, email TEXT);
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, title TEXT NOT NULL, date TEXT NOT NULL, location TEXT);
CREATE TABLE IF NOT EXISTS rsvps(event_id INTEGER, member_id INTEGER, PRIMARY KEY(event_id, member_id));
CREATE TABLE IF NOT EXISTS dues(id INTEGER PRIMARY KEY, member_id INTEGER NOT NULL, label TEXT NOT NULL, amount_cents INTEGER NOT NULL, paid INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS announcements(id INTEGER PRIMARY KEY, body TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP);
";

const CSS: &str = "body{margin:0;font-family:system-ui,sans-serif;background:#0f1222;color:#e8e8f0}\
header{display:flex;gap:1rem;align-items:center;padding:.8rem 1.2rem;background:#1b1f3a;border-bottom:2px solid #c9a227}\
header nav{flex:1;display:flex;gap:1rem}a{color:#c9a227;text-decoration:none}\
main{max-width:860px;margin:0 auto;padding:1rem}.card{background:#1b1f3a;padding:.8rem 1rem;margin:.5rem 0;border-radius:8px}\
small{display:block;opacity:.6}table{width:100%;border-collapse:collapse}td,th{padding:.4rem;text-align:left;border-bottom:1px solid #2c3157}\
input,select,textarea,button{padding:.5rem;margin:.2rem;border-radius:6px;border:1px solid #444;background:#12152b;color:inherit}\
button{background:#c9a227;color:#000;border:0;cursor:pointer}form.row{display:flex;flex-wrap:wrap}.paid{color:#5ecb7a}.owed{color:#ff7a7a}";

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    member_pw: String,
    admin_pw: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Member,
    Admin,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn money(c: i64) -> String {
    format!("${}.{:02}", c / 100, c % 100)
}

fn page(role: Role, title: &str, body: String) -> Html<String> {
    let badge = if role == Role::Admin { "EXEC" } else { "BROTHER" };
    let t = esc(title);
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t}</title><style>{CSS}</style></head><body><header><b>Chapter Hub</b><nav><a href="/">Home</a><a href="/members">Members</a><a href="/events">Events</a><a href="/dues">Dues</a></nav><small>{badge}</small></header><main><h1>{t}</h1>{body}</main></body></html>"#
    ))
}

fn admin_only(role: Role) -> Option<Response> {
    (role != Role::Admin).then(|| StatusCode::FORBIDDEN.into_response())
}

async fn auth(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let pw = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| B64.decode(b).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.split_once(':').map(|(_, p)| p.to_string()));
    let role = match pw {
        Some(p) if p == st.admin_pw => Role::Admin,
        Some(p) if p == st.member_pw => Role::Member,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Basic realm=\"chapter\"")],
                "Login required",
            )
                .into_response()
        }
    };
    req.extensions_mut().insert(role);
    next.run(req).await
}

// ---------- Home ----------
async fn home(State(st): State<AppState>, Extension(role): Extension<Role>) -> Html<String> {
    let ann: Vec<(String, String)> =
        sqlx::query_as("SELECT body, created_at FROM announcements ORDER BY id DESC LIMIT 10")
            .fetch_all(&st.db).await.unwrap_or_default();
    let ev: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT title, date, COALESCE(location,'') FROM events WHERE date >= date('now') ORDER BY date LIMIT 5",
    )
    .fetch_all(&st.db).await.unwrap_or_default();

    let mut b = String::new();
    if role == Role::Admin {
        b.push_str(r#"<form method="post" action="/announce"><textarea name="body" placeholder="Post an announcement" required style="width:100%"></textarea><button>Post</button></form>"#);
    }
    b.push_str("<h2>Announcements</h2>");
    for (body, at) in ann {
        b.push_str(&format!("<div class=card>{}<small>{}</small></div>", esc(&body), esc(&at)));
    }
    b.push_str("<h2>Upcoming events</h2>");
    for (t, d, l) in ev {
        b.push_str(&format!("<div class=card><b>{}</b><small>{} · {}</small></div>", esc(&t), esc(&d), esc(&l)));
    }
    page(role, "Home", b)
}

#[derive(Deserialize)]
struct Announce {
    body: String,
}

async fn announce(State(st): State<AppState>, Extension(role): Extension<Role>, Form(f): Form<Announce>) -> Response {
    if let Some(r) = admin_only(role) { return r; }
    let _ = sqlx::query("INSERT INTO announcements(body) VALUES(?)").bind(f.body).execute(&st.db).await;
    Redirect::to("/").into_response()
}

// ---------- Members ----------
async fn members(State(st): State<AppState>, Extension(role): Extension<Role>) -> Html<String> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT name, COALESCE(pledge_class,''), COALESCE(title,''), COALESCE(email,'') FROM members ORDER BY name",
    )
    .fetch_all(&st.db).await.unwrap_or_default();
    let mut b = String::new();
    if role == Role::Admin {
        b.push_str(r#"<form class="row" method="post" action="/members"><input name="name" placeholder="Name" required><input name="pledge_class" placeholder="Pledge class"><input name="title" placeholder="Position"><input name="email" placeholder="Email"><button>Add brother</button></form>"#);
    }
    b.push_str("<table><tr><th>Name</th><th>Class</th><th>Position</th><th>Email</th></tr>");
    for (n, p, t, e) in rows {
        b.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>", esc(&n), esc(&p), esc(&t), esc(&e)));
    }
    b.push_str("</table>");
    page(role, "Members", b)
}

#[derive(Deserialize)]
struct NewMember {
    name: String,
    pledge_class: String,
    title: String,
    email: String,
}

async fn add_member(State(st): State<AppState>, Extension(role): Extension<Role>, Form(f): Form<NewMember>) -> Response {
    if let Some(r) = admin_only(role) { return r; }
    let _ = sqlx::query("INSERT INTO members(name,pledge_class,title,email) VALUES(?,?,?,?)")
        .bind(f.name).bind(f.pledge_class).bind(f.title).bind(f.email)
        .execute(&st.db).await;
    Redirect::to("/members").into_response()
}

// ---------- Events ----------
async fn member_options(db: &SqlitePool, with_all: bool) -> String {
    let ms: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM members ORDER BY name")
        .fetch_all(db).await.unwrap_or_default();
    let mut o = String::new();
    if with_all { o.push_str("<option value=0>All members</option>"); }
    for (id, n) in ms { o.push_str(&format!("<option value={id}>{}</option>", esc(&n))); }
    o
}

async fn events(State(st): State<AppState>, Extension(role): Extension<Role>) -> Html<String> {
    let evs: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, title, date, COALESCE(location,'') FROM events ORDER BY date DESC",
    )
    .fetch_all(&st.db).await.unwrap_or_default();
    let opts = member_options(&st.db, false).await;

    let mut b = String::new();
    if role == Role::Admin {
        b.push_str(r#"<form class="row" method="post" action="/events"><input name="title" placeholder="Event" required><input type="date" name="date" required><input name="location" placeholder="Location"><button>Create</button></form>"#);
    }
    for (id, t, d, l) in evs {
        let going: Vec<(String,)> = sqlx::query_as(
            "SELECT m.name FROM rsvps r JOIN members m ON m.id = r.member_id WHERE r.event_id = ? ORDER BY m.name",
        )
        .bind(id).fetch_all(&st.db).await.unwrap_or_default();
        let names: Vec<String> = going.iter().map(|(n,)| esc(n)).collect();
        b.push_str(&format!(
            "<div class=card><b>{}</b><small>{} · {}</small><small>{} going: {}</small>\
             <form class=row method=post action=\"/events/{id}/rsvp\"><select name=member_id>{opts}</select><button>RSVP</button></form></div>",
            esc(&t), esc(&d), esc(&l), names.len(), names.join(", ")
        ));
    }
    page(role, "Events", b)
}

#[derive(Deserialize)]
struct NewEvent {
    title: String,
    date: String,
    location: String,
}

async fn add_event(State(st): State<AppState>, Extension(role): Extension<Role>, Form(f): Form<NewEvent>) -> Response {
    if let Some(r) = admin_only(role) { return r; }
    let _ = sqlx::query("INSERT INTO events(title,date,location) VALUES(?,?,?)")
        .bind(f.title).bind(f.date).bind(f.location).execute(&st.db).await;
    Redirect::to("/events").into_response()
}

#[derive(Deserialize)]
struct Rsvp {
    member_id: i64,
}

async fn rsvp(State(st): State<AppState>, Path(id): Path<i64>, Form(f): Form<Rsvp>) -> Response {
    let _ = sqlx::query("INSERT OR IGNORE INTO rsvps(event_id, member_id) VALUES(?,?)")
        .bind(id).bind(f.member_id).execute(&st.db).await;
    Redirect::to("/events").into_response()
}

// ---------- Dues ----------
async fn dues(State(st): State<AppState>, Extension(role): Extension<Role>) -> Html<String> {
    let rows: Vec<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT d.id, m.name, d.label, d.amount_cents, d.paid FROM dues d JOIN members m ON m.id = d.member_id ORDER BY d.paid, m.name",
    )
    .fetch_all(&st.db).await.unwrap_or_default();
    let collected: i64 = rows.iter().filter(|r| r.4 == 1).map(|r| r.3).sum();
    let owed: i64 = rows.iter().filter(|r| r.4 == 0).map(|r| r.3).sum();

    let mut b = format!("<div class=card>Collected <b class=paid>{}</b> · Outstanding <b class=owed>{}</b></div>", money(collected), money(owed));
    if role == Role::Admin {
        let opts = member_options(&st.db, true).await;
        b.push_str(&format!(r#"<form class="row" method="post" action="/dues"><select name="member_id">{opts}</select><input name="label" placeholder="Fall dues" required><input name="amount" type="number" step="0.01" placeholder="Amount" required><button>Charge</button></form>"#));
    }
    b.push_str("<table><tr><th>Brother</th><th>For</th><th>Amount</th><th></th></tr>");
    for (id, n, l, c, paid) in rows {
        let status = if paid == 1 {
            "<span class=paid>Paid</span>".to_string()
        } else if role == Role::Admin {
            format!("<form method=post action=\"/dues/{id}/paid\"><button>Mark paid</button></form>")
        } else {
            "<span class=owed>Due</span>".to_string()
        };
        b.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{status}</td></tr>", esc(&n), esc(&l), money(c)));
    }
    b.push_str("</table>");
    page(role, "Dues", b)
}

#[derive(Deserialize)]
struct NewDue {
    member_id: i64,
    label: String,
    amount: f64,
}

async fn add_due(State(st): State<AppState>, Extension(role): Extension<Role>, Form(f): Form<NewDue>) -> Response {
    if let Some(r) = admin_only(role) { return r; }
    let cents = (f.amount * 100.0).round() as i64;
    if f.member_id == 0 {
        let _ = sqlx::query("INSERT INTO dues(member_id,label,amount_cents) SELECT id, ?, ? FROM members")
            .bind(f.label).bind(cents).execute(&st.db).await;
    } else {
        let _ = sqlx::query("INSERT INTO dues(member_id,label,amount_cents) VALUES(?,?,?)")
            .bind(f.member_id).bind(f.label).bind(cents).execute(&st.db).await;
    }
    Redirect::to("/dues").into_response()
}

async fn mark_paid(State(st): State<AppState>, Extension(role): Extension<Role>, Path(id): Path<i64>) -> Response {
    if let Some(r) = admin_only(role) { return r; }
    let _ = sqlx::query("UPDATE dues SET paid = 1 WHERE id = ?").bind(id).execute(&st.db).await;
    Redirect::to("/dues").into_response()
}

#[tokio::main]
async fn main() {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://chapter.db?mode=rwc".into());
    let db = SqlitePoolOptions::new().connect(&url).await.expect("db connect");
    sqlx::raw_sql(SCHEMA).execute(&db).await.expect("schema");

    let st = AppState {
        db,
        member_pw: std::env::var("MEMBER_PASSWORD").unwrap_or_else(|_| "brothers".into()),
        admin_pw: std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "exec".into()),
    };

    let app = Router::new()
        .route("/", get(home))
        .route("/announce", post(announce))
        .route("/members", get(members).post(add_member))
        .route("/events", get(events).post(add_event))
        .route("/events/:id/rsvp", post(rsvp))
        .route("/dues", get(dues).post(add_due))
        .route("/dues/:id/paid", post(mark_paid))
        .layer(middleware::from_fn_with_state(st.clone(), auth))
        .with_state(st);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Chapter Hub running on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
