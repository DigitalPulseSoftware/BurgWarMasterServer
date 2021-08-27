use std::net::IpAddr;
use std::str::FromStr;
use std::{u16, usize};

use actix_web::HttpResponse;
use actix_web::{get, middleware, post, web, App, HttpRequest, HttpServer};
use r2d2_redis::redis::{Commands, FromRedisValue};
use r2d2_redis::{r2d2, redis, RedisConnectionManager};
use rand::Rng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MASTER_SERVER_DATA_VERSION: u32 = 1;

const MAX_DESCRIPTION_LENGTH: usize = 1024;
const MAX_GAMEMODE_LENGTH: usize = 255;
const MAX_MAP_LENGTH: usize = 255;
const MAX_MOD_COUNT: usize = 255;
const MAX_MOD_NAME_LENGTH: usize = 32;
const MAX_NAME_LENGTH: usize = 32;
const MAX_TAG_COUNT: usize = 8;
const MAX_TAG_NAME_LENGTH: usize = 16;

const SERVER_EXPIRE_TIME: usize = 60;

#[derive(Clone, Serialize, Deserialize)]
struct AppConfig {
    listen_address: String,
    listen_port: u16,
    ipv4_fallback_url: Option<String>,
    redis_uri: String,
}

impl Default for AppConfig {
    fn default() -> Self { 
        Self {
            listen_address: "0.0.0.0".to_owned(),
            listen_port: 8080,
            ipv4_fallback_url: None,
            redis_uri: "localhost:6379".to_owned()
        } 
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RegisterServerInfo {
    data_version: u32,
    game_version: u32,
    update_token: Option<String>,
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
        let mut hash = vec![
            ("version".to_owned(), self.game_version.to_string()),
            ("port".to_owned(), self.port.to_string()),
            ("name".to_owned(), self.name.clone()),
            ("gamemode".to_owned(), self.gamemode.clone()),
            ("map".to_owned(), self.map.clone()),
            (
                "current_player_count".to_owned(),
                self.current_player_count.to_string(),
            ),
            (
                "maximum_player_count".to_owned(),
                self.maximum_player_count.to_string(),
            ),
        ];

        if let Some(ref desc) = self.description {
            hash.push(("description".to_owned(), desc.clone()));
        }

        if let Some(ref mods) = self.mods {
            hash.push(("mods".to_owned(), mods.join(" ")));
        }

        if let Some(ref tags) = self.tags {
            hash.push(("tags".to_owned(), tags.join(" ")));
        }

        hash
    }

    fn validate(&self) -> Option<String> {
        if self.data_version != MASTER_SERVER_DATA_VERSION {
            return Some("incompatible data version".to_string());
        }
        if self.name.is_empty() {
            return Some("name cannot be empty".to_string());
        }
        if self.name.len() > MAX_NAME_LENGTH {
            return Some("name is too long".to_string());
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
                if mod_name.contains(' ') {
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
                if tag_name.contains(' ') {
                    return Some("Tags cannot contain spaces".to_string());
                }
            }
        }

        None
    }
}

#[derive(Clone, Debug, Serialize)]
struct ServerCreationData {
    data_version: u32,
    register_ipv4_url: Option<String>,
    token: String,
}

#[post("/servers")]
async fn create_server(
    app_config: web::Data<AppConfig>,
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
            addr = ip;
        } else {
            addr = req.peer_addr().unwrap().ip();
        }
    } else {
        addr = req.peer_addr().unwrap().ip();
    }

    let mut conn = redis_pool.get().unwrap();

    if let Some(update_token) = &server_info.update_token {
        let server_key = "SERVERS:".to_owned() + update_token;

        let hash = server_info.to_redis_hash();

        let redis_result: Result<redis::Value, redis::RedisError> =
            redis::transaction(&mut *conn, &[server_key.as_str()], |conn, pipe| {
                let uuid: String = conn.hget(&server_key, "uuid")?;
                let uuid_key = "SERVER_BY_UUID:".to_owned() + &uuid;

                pipe.atomic()
                    .expire(&server_key, SERVER_EXPIRE_TIME)
                    .expire(&uuid_key, SERVER_EXPIRE_TIME)
                    .ignore()
                    .hset_multiple(&server_key, &hash)
                    .ignore()
                    .query(conn)
            });

        let result = ServerCreationData {
            data_version: MASTER_SERVER_DATA_VERSION,
            register_ipv4_url: None,
            token: update_token.to_string(),
        };

        match redis_result {
            Ok(_) => match serde_json::to_value(result) {
                Ok(json) => HttpResponse::Ok().body(json.to_string()),
                Err(err) => {
                    println!("server creation data serialization failed: {}", err);
                    HttpResponse::InternalServerError().body("serialization failed")
                }
            },
            Err(err) => match err.kind() {
                redis::ErrorKind::TypeError => {
                    return HttpResponse::NotFound().body("not found");
                }
                _ => {
                    println!("an unexpected error occurred: {}", err);
                    return HttpResponse::InternalServerError()
                        .body("failed to retrieve server info");
                }
            },
        }
    } else {
        let public_uuid = Uuid::new_v4().to_string();
        let private_key = rand::thread_rng().gen::<[u8; 24]>();
        let private_key_b64 = base64::encode(&private_key);

        let server_key = "SERVERS:".to_owned() + &private_key_b64;
        let uuid_key = "SERVER_BY_UUID:".to_owned() + &public_uuid;

        let mut hash = server_info.to_redis_hash();
        hash.push(("uuid".to_owned(), public_uuid));
        hash.push(("ip".to_owned(), addr.to_string()));

        let redis_result: Result<redis::Value, redis::RedisError> = redis::pipe()
            .atomic()
            .hset_multiple(&server_key, &hash)
            .set_ex(uuid_key, &private_key_b64, SERVER_EXPIRE_TIME)
            .ignore()
            .expire(&server_key, SERVER_EXPIRE_TIME)
            .query(&mut *conn);

        let ipv4_url = if addr.is_ipv6() {
            if let Some(url) = &app_config.ipv4_fallback_url {
                Some(format!("{}/servers/register_ipv4", url))
            }
            else {
                None
            }
        }
        else {
            None
        };

        let result = ServerCreationData {
            data_version: MASTER_SERVER_DATA_VERSION,
            register_ipv4_url: ipv4_url,
            token: private_key_b64,
        };

        match redis_result {
            Ok(_) => match serde_json::to_value(result) {
                Ok(json) => HttpResponse::Ok().body(json.to_string()),
                Err(err) => {
                    println!("server creation data serialization failed: {}", err);
                    HttpResponse::InternalServerError().body("serialization failed")
                }
            },
            Err(err) => {
                println!("A redis error occurred: {}", err);
                HttpResponse::InternalServerError().body("An error occurred")
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct UpdateIpv4Info {
    update_token: String,
}

#[post("/servers/register_ipv4")]
async fn update_server_ipv4(
    redis_pool: web::Data<r2d2::Pool<RedisConnectionManager>>,
    server_info: web::Json<UpdateIpv4Info>,
    req: HttpRequest,
) -> HttpResponse {
    let addr;
    if let Some(forward_ip) = req.connection_info().realip_remote_addr() {
        if let Ok(ip) = IpAddr::from_str(forward_ip) {
            addr = ip;
        } else {
            addr = req.peer_addr().unwrap().ip();
        }
    } else {
        addr = req.peer_addr().unwrap().ip();
    }

    if !addr.is_ipv4() {
        return HttpResponse::BadRequest().body("this route must be called using an IPv4");
    }

    let mut conn = redis_pool.get().unwrap();

    let server_key = "SERVERS:".to_owned() + &server_info.update_token;

    let redis_result: Result<redis::Value, redis::RedisError> =
        redis::transaction(&mut *conn, &[server_key.as_str()], |conn, pipe| {
            let mut ip: String = conn.hget(&server_key, "ip")?;
            ip = ip + &";".to_owned() + &addr.to_string();

            pipe.atomic()
                .hset(&server_key, "ip", ip)
                .ignore()
                .query(conn)
        });

    match redis_result {
        Ok(_) => HttpResponse::Ok().body("{}"),
        Err(err) => match err.kind() {
            redis::ErrorKind::TypeError => {
                return HttpResponse::NotFound().body("not found");
            }
            _ => {
                println!("an unexpected error occurred: {}", err);
                return HttpResponse::InternalServerError()
                    .body("failed to retrieve server info");
            }
        },
    }
}

#[derive(Clone, Debug, Serialize)]
struct ServerConnectionInfo {
    data_version: u32,
    ip: String,
    port: u16,
    is_local: bool,
}

#[get("/server/{uuid}/connection_details")]
async fn connect_to_server(
    web::Path(uuid): web::Path<String>,
    redis_pool: web::Data<r2d2::Pool<RedisConnectionManager>>,
    req: HttpRequest,
) -> HttpResponse {
    let addr;
    if let Some(forward_ip) = req.connection_info().realip_remote_addr() {
        if let Ok(ip) = IpAddr::from_str(forward_ip) {
            addr = ip;
        } else {
            addr = req.peer_addr().unwrap().ip();
        }
    } else {
        addr = req.peer_addr().unwrap().ip();
    }

    let mut conn = redis_pool.get().unwrap();

    let result = redis::transaction(&mut *conn, &[uuid.as_str()], |conn, pipe| {
        let server_b64: String = conn.get("SERVER_BY_UUID:".to_owned() + &uuid)?;
        let server_key = "SERVERS:".to_owned() + &server_b64;

        pipe.atomic()
            .hget(&server_key, "ip")
            .hget(&server_key, "port")
            .query(conn)
    });

    if let Err(err) = result {
        match err.kind() {
            redis::ErrorKind::TypeError => {
                return HttpResponse::NotFound().body("not found");
            }
            _ => {
                println!("an unexpected error occurred: {}", err);
                return HttpResponse::InternalServerError().body("failed to retrieve server info");
            }
        }
    }

    let (ip, port) = result.unwrap();

    let result = ServerConnectionInfo {
        data_version: MASTER_SERVER_DATA_VERSION,
        is_local: ip == addr.to_string(),
        ip,
        port,
    };

    match serde_json::to_value(result) {
        Ok(json) => HttpResponse::Ok().body(json.to_string()),
        Err(err) => {
            println!("server connection info serialization failed: {}", err);
            HttpResponse::InternalServerError().body("serialization failed")
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ServerInfo {
    game_version: u32,
    uuid: String,
    name: String,
    has_password: Option<bool>,
    description: String,
    gamemode: String,
    map: String,
    current_player_count: u16,
    maximum_player_count: u16,
    uptime: u64,
    mods: Vec<String>,
    tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ServerListInfo {
    data_version: u32,
    servers: Vec<ServerInfo>,
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

    let mut result = ServerListInfo {
        data_version: MASTER_SERVER_DATA_VERSION,
        servers: Vec::new(),
    };

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
                            server_info.description = String::from_redis_value(value).unwrap()
                        }
                        "gamemode" => {
                            server_info.gamemode = String::from_redis_value(value).unwrap()
                        }
                        "map" => server_info.map = String::from_redis_value(value).unwrap(),
                        "maximum_player_count" => {
                            server_info.maximum_player_count = u16::from_redis_value(value).unwrap()
                        }
                        "mods" => {
                            let mod_list = String::from_redis_value(value).unwrap();
                            for mod_name in mod_list.split_whitespace() {
                                server_info.mods.push(mod_name.to_owned());
                            }
                        }
                        "uuid" => server_info.uuid = String::from_redis_value(value).unwrap(),
                        "name" => server_info.name = String::from_redis_value(value).unwrap(),
                        "tags" => {
                            let tag_list = String::from_redis_value(value).unwrap();
                            for tag_name in tag_list.split_whitespace() {
                                server_info.tags.push(tag_name.to_owned());
                            }
                        }
                        "version" => {
                            server_info.game_version = u32::from_redis_value(value).unwrap()
                        }
                        // Don't send ip and port
                        "ip" => {}
                        "port" => {}
                        _ => {
                            println!("unknown server field {} from redis", field);
                        }
                    }
                }

                result.servers.push(server_info);
            }
            Err(err) => println!("failed to retrieve {}: {}", key, err),
        }
    }

    match serde_json::to_value(result) {
        Ok(json) => HttpResponse::Ok().body(json.to_string()),
        Err(err) => {
            println!("server list serialization failed: {}", err);
            HttpResponse::InternalServerError().body("serialization failed")
        }
    }
}

#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    let config: AppConfig = confy::load_path("bw_master_server.toml").unwrap();

    std::env::set_var("RUST_LOG", "info,actix_web=info");
    env_logger::init();

    let manager = RedisConnectionManager::new("redis://".to_owned() + &config.redis_uri).unwrap();
    let pool = r2d2::Pool::builder().build(manager).unwrap();

    let bind_address = format!("{}:{}", &config.listen_address, &config.listen_port);

    HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .data(config.clone())
            .data(pool.clone())
            .service(create_server)
            .service(index)
            .service(connect_to_server)
            .service(update_server_ipv4)
    })
    .bind(bind_address)?
    //.bind("[::]:8080")?
    .run()
    .await
}
