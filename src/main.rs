use std::{u16, usize};
use std::net::IpAddr;
use std::str::FromStr;

use actix_web::HttpResponse;
use actix_web::{get, middleware, post, web, App, HttpRequest, HttpServer};
use r2d2_redis::redis::{Commands, FromRedisValue};
use r2d2_redis::{r2d2, redis, RedisConnectionManager};
use rand::Rng;
use serde::{Deserialize, Serialize};

const MAX_DESCRIPTION_LENGTH: usize = 1024;
const MAX_GAMEMODE_LENGTH: usize = 255;
const MAX_MAP_LENGTH: usize = 255;
const MAX_MOD_COUNT: usize = 255;
const MAX_MOD_NAME_LENGTH: usize = 32;
const MAX_TAG_COUNT: usize = 8;
const MAX_TAG_NAME_LENGTH: usize = 16;

const SERVER_EXPIRE_TIME: usize = 60;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ServerInfo {
    version: u32,
    name: String,
    address: Option<IpAddr>,
    has_password: Option<bool>,
    desc: String,
    gamemode: String,
    map: String,
    current_player_count: u16,
    maximum_player_count: u16,
    port: u16,
    uptime: u64,
    mods: Vec<String>,
    tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RegisterServerInfo {
    update_token: Option<String>,
    version: u32,
    name: String,
    has_password: Option<bool>,
    description: Option<String>,
    gamemode: String,
    map: String,
    current_player_count: u16,
    maximum_player_count: u16,
    port: u16,
    uptime: u64,
    mods: Option<Vec<String>>,
    tags: Option<Vec<String>>,
}

impl RegisterServerInfo {
    fn to_redis_hash(&self) -> Vec<(String, String)> {
        let mut hash = vec![];
        hash.push(("version".to_owned(), self.version.to_string()));
        hash.push(("port".to_owned(), self.port.to_string()));
        hash.push(("name".to_owned(), self.name.clone()));
        if let Some(ref desc) = self.description {
            hash.push(("description".to_owned(), desc.clone()));
        }

        hash.push(("gamemode".to_owned(), self.gamemode.clone()));
        hash.push(("map".to_owned(), self.map.clone()));
        hash.push((
            "current_player_count".to_owned(),
            self.current_player_count.to_string(),
        ));
        hash.push((
            "maximum_player_count".to_owned(),
            self.maximum_player_count.to_string(),
        ));

        if let Some(ref mods) = self.mods {
            hash.push(("mods".to_owned(), mods.join(" ")));
        }

        if let Some(ref tags) = self.tags {
            hash.push(("tags".to_owned(), tags.join(" ")));
        }

        hash
    }

    fn validate(&self) -> Option<String> {
        if self.name.is_empty() {
            return Some("name cannot be empty".to_string());
        }
        if let Some(ref desc) = self.description {
            if desc.len() >= MAX_DESCRIPTION_LENGTH {
                return Some("server description is too long".to_string());
            }
        }
        if self.gamemode.is_empty() {
            return Some("gamemode cannot be empty".to_string());
        }
        if self.gamemode.len() > MAX_GAMEMODE_LENGTH {
            return Some("map name is too long".to_string());
        }
        if self.map.is_empty() {
            return Some("gamemode cannot be empty".to_string());
        }
        if self.map.len() > MAX_MAP_LENGTH {
            return Some("map name is too long".to_string());
        }
        if self.current_player_count > self.maximum_player_count {
            return Some("invalid player count".to_string());
        }
        if self.port == 0 {
            return Some("invalid port".to_string());
        }
        if let Some(ref mods) = self.mods {
            if mods.len() > MAX_MOD_COUNT {
                return Some("too many mods".to_string());
            }
            for mod_name in mods.iter() {
                if mod_name.is_empty() {
                    return Some("empty mod name".to_string());
                }
                if mod_name.len() > MAX_MOD_NAME_LENGTH {
                    return Some(format!("mod name \"{0}\" is too long", mod_name));
                }
                if mod_name.contains(" ") {
                    return Some("Mods cannot contain spaces".to_string());
                }
            }
        }
        if let Some(ref tags) = self.tags {
            if tags.len() > MAX_TAG_COUNT {
                return Some("too many tags".to_string());
            }
            for tag_name in tags.iter() {
                if tag_name.is_empty() {
                    return Some("empty tag name".to_string());
                }
                if tag_name.len() > MAX_TAG_NAME_LENGTH {
                    return Some(format!("tag name \"{0}\" is too long", tag_name));
                }
                if tag_name.contains(" ") {
                    return Some("Tags cannot contain spaces".to_string());
                }
            }
        }

        None
    }
}

#[post("/servers")]
async fn create_server(
    redis_pool: web::Data<r2d2::Pool<RedisConnectionManager>>,
    server_info: web::Json<RegisterServerInfo>,
    req: HttpRequest,
) -> HttpResponse {
    if let Some(err) = server_info.validate() {
        return HttpResponse::BadRequest().body(err);
    }

    let addr;
    if let Some(forward_ip) = req.connection_info().realip_remote_addr() {
        if let Ok(ip) = IpAddr::from_str(forward_ip) {
            addr = ip.to_string();
        } else {
            addr = req.peer_addr().unwrap().ip().to_string();
        }
    } else {
        addr = req.peer_addr().unwrap().ip().to_string();
    }

    let mut conn = redis_pool.get().unwrap();

    if let Some(update_token) = &server_info.update_token {
        let server_key = "SERVERS:".to_owned() + &update_token;

        let mut hash = server_info.to_redis_hash();
        hash.push(("ip".to_owned(), addr));

        let result: Result<redis::Value, redis::RedisError> =
            redis::transaction(&mut *conn, &[server_key.as_str()], |conn, pipe| {
                let seconds: isize = conn.ttl(&server_key).unwrap();
                if seconds <= 0 {
                    return Err(redis::RedisError::from((
                        redis::ErrorKind::ExtensionError,
                        "ServerKeyDoesNotExist",
                    )));
                }

                pipe.atomic()
                    .expire(&server_key, SERVER_EXPIRE_TIME)
                    .ignore()
                    .hset_multiple(&server_key, &hash)
                    .ignore()
                    .query(conn)
            });

        match result {
            Ok(_) => HttpResponse::Ok().body(update_token),
            Err(err) => {
                if err.to_string() == "ServerKeyDoesNotExist" {
                    return HttpResponse::NotFound().body("Invalid update token");
                }
                println!("A redis error occurred: {}", err);
                HttpResponse::InternalServerError().body("An error occurred")
            }
        }
    } else {
        let private_key = rand::thread_rng().gen::<[u8; 24]>();
        let private_key_b64 = base64::encode(&private_key);

        let server_key = "SERVERS:".to_owned() + &private_key_b64;

        let mut hash = server_info.to_redis_hash();
        hash.push(("ip".to_owned(), addr));

        let result: Result<redis::Value, redis::RedisError> = redis::pipe()
            .atomic()
            .hset_multiple(&server_key, &hash)
            .ignore()
            .expire(&server_key, SERVER_EXPIRE_TIME)
            .query(&mut *conn);

        match result {
            Ok(_) => HttpResponse::Ok().body(private_key_b64),
            Err(err) => {
                println!("A redis error occurred: {}", err);
                HttpResponse::InternalServerError().body("An error occurred")
            }
        }
    }
}

#[get("/servers")]
async fn index(redis_pool: web::Data<r2d2::Pool<RedisConnectionManager>>) -> HttpResponse {
    let mut conn = redis_pool.get().unwrap();

    let iter: redis::Iter<String> = redis::cmd("SCAN")
        .cursor_arg(0)
        .arg("MATCH")
        .arg("SERVERS:*")
        .clone()
        .iter(&mut *conn)
        .unwrap();

    // Read all keys first
    let mut server_keys = Vec::new();
    for key in iter {
        server_keys.push(key);
    }

    let mut server_list = Vec::new();
    for key in server_keys.iter() {
        let keys: Result<redis::Value, redis::RedisError> = conn.hgetall(key);
        match keys {
            Ok(keys) => {
                let mut server_info: ServerInfo = ServerInfo::default();

                for (field, value) in keys.as_map_iter().unwrap() {
                    let field: String = String::from_redis_value(field).unwrap();

                    match field.as_str() {
                        "current_player_count" => {
                            server_info.current_player_count = u16::from_redis_value(value).unwrap()
                        }
                        "description" => {
                            server_info.desc = String::from_redis_value(value).unwrap()
                        }
                        "gamemode" => {
                            server_info.gamemode = String::from_redis_value(value).unwrap()
                        }
                        "map" => server_info.map = String::from_redis_value(value).unwrap(),
                        "ip" => {
                            server_info.address = Some(
                                IpAddr::from_str(String::from_redis_value(value).unwrap().as_str())
                                    .unwrap(),
                            )
                        }
                        "maximum_player_count" => {
                            server_info.maximum_player_count = u16::from_redis_value(value).unwrap()
                        }
                        "mods" => {
                            let mod_list = String::from_redis_value(value).unwrap();
                            for mod_name in mod_list.split_whitespace() {
                                server_info.mods.push(mod_name.to_owned());
                            }
                        }
                        "name" => server_info.name = String::from_redis_value(value).unwrap(),
                        "port" => server_info.port = u16::from_redis_value(value).unwrap(),
                        "tags" => {
                            let tag_list = String::from_redis_value(value).unwrap();
                            for tag_name in tag_list.split_whitespace() {
                                server_info.tags.push(tag_name.to_owned());
                            }
                        }
                        "version" => server_info.version = u32::from_redis_value(value).unwrap(),
                        _ => {
                            println!("unknown server field {} from redis", field);
                        }
                    }
                }

                server_list.push(server_info);
            }
            Err(err) => println!("failed to retrieve {}: {}", key, err),
        }
    }

    match serde_json::to_value(server_list) {
        Ok(json) => HttpResponse::Ok().body(json.to_string()),
        Err(err) => {
            println!("server list serialization failed: {}", err);
            HttpResponse::InternalServerError().body("serialization failed")
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "info,actix_web=info");
    env_logger::init();

    let manager = RedisConnectionManager::new("redis://localhost").unwrap();
    let pool = r2d2::Pool::builder().build(manager).unwrap();

    HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .data(pool.clone())
            .service(create_server)
            .service(index)
    })
    .bind("0.0.0.0:8080")?
    .bind("[::]:8080")?
    .run()
    .await
}
