use std::collections::HashMap;

use actix_web::{middleware, web, App, HttpServer, Responder, Result};
use actix_web_lab::respond::Html;
use askama::Template;

#[derive(Template)]
#[template(path = "user.html")]
struct UserTemplate<'a> {
    name: &'a str,
    text: &'a str,
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index;

async fn index(query: web::Query<HashMap<String, String>>) -> Result<impl Responder> {
    let f = std::fs::File::open("../mirror/depends.json").unwrap();
    let depends: zerus::TopLevelDepends = serde_json::from_reader(f).unwrap();

    let mut crates = Crates { crates: vec![] };
    for depend in depends.depends {
        crates.crates.push(CrateInfo {
            name: depend.name,
            version: depend.version,
        });
    }

    #[derive(Template)]
    #[template(path = "crates.html")]
    struct Crates {
        crates: Vec<CrateInfo>,
    }
    struct CrateInfo {
        name: String,
        version: String,
    }
    let html = crates.render().unwrap();

    Ok(Html(html))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    log::info!("starting HTTP server at http://localhost:8080");

    HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .service(web::resource("/").route(web::get().to(index)))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
