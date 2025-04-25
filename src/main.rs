use actix_files::Files;
use actix_web::{
    App, HttpResponse, HttpServer, Responder,
    cookie::{Cookie, SameSite},
    web,
};
use config::deserializer::Deserializer;
use dash_to_hls::DashToHlsConverter;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod auth;
mod config;
mod dash_to_hls;

// Stream management structures
struct StreamManager {
    streams: HashMap<String, StreamInfo>,
    active_streams: HashMap<String, Arc<Mutex<DashToHlsConverter>>>,
    last_access: HashMap<String, Instant>,
}

#[derive(Clone)]
struct StreamInfo {
    id: String,
    name: String,
    url: String,
    key: String,
    init_segments: HashMap<String, Vec<u8>>,
}

#[derive(Serialize)]
struct ChannelInfo {
    id: String,
    name: String,
}

struct UserManager {
    users: HashMap<String, String>,
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login(
    req: web::Json<LoginRequest>,
    user_manager: web::Data<Arc<Mutex<UserManager>>>,
) -> impl Responder {
    let user_manager = user_manager.lock().unwrap();

    if let Some(pass) = user_manager.users.get(&req.username) {
        if req.password == *pass {
            match auth::create_token(&req.username) {
                Ok(token) => {
                    let cookie = Cookie::build("auth", token)
                        .http_only(true)
                        .same_site(SameSite::Lax)
                        .secure(false) // Set to true in production with HTTPS!
                        .path("/")
                        .finish();

                    return HttpResponse::Ok()
                        .cookie(cookie)
                        .json(serde_json::json!({ "message": "Logged in" }));
                }
                Err(_) => return HttpResponse::InternalServerError().finish(),
            }
        }
    }

    HttpResponse::Unauthorized().body("Invalid credentials")
}

async fn proxy_stream(
    _user: auth::AuthenticatedUser,
    path: web::Path<(String, String)>,
    stream_manager: web::Data<Arc<Mutex<StreamManager>>>,
) -> impl Responder {
    let (stream_name, file_path) = path.into_inner();

    let mut stream_manager = stream_manager.lock().unwrap();

    if let Some(_) = stream_manager.active_streams.get(&stream_name) {
        stream_manager
            .last_access
            .insert(stream_name.clone(), Instant::now());
    } else {
        return HttpResponse::NotFound().body("Stream not active");
    }

    let stream_info = match stream_manager.streams.get(&stream_name) {
        Some(info) => info,
        None => return HttpResponse::NotFound().body("Stream not found"),
    };

    if file_path.ends_with(".m3u8") {
        let file_content =
            fs::read_to_string(format!("./streams/{}/{}", stream_info.id, file_path))
                .unwrap_or_else(|_| "".to_string());

        HttpResponse::Ok()
            .content_type("application/vnd.apple.mpegurl")
            .body(file_content)
    } else if file_path.ends_with(".ts") || file_path.ends_with(".m4s") {
        match fs::read(format!("./streams/{}/{}", stream_info.id, file_path)) {
            Ok(data) => HttpResponse::Ok().content_type("video/mp2t").body(data),
            Err(_) => HttpResponse::NotFound().body("Segment not found"),
        }
    } else {
        HttpResponse::BadRequest().body("Invalid file type")
    }
}

async fn initialize_stream(
    _user: auth::AuthenticatedUser,
    stream_name: web::Path<String>,
    stream_manager: web::Data<Arc<Mutex<StreamManager>>>,
) -> impl Responder {
    let stream_name = stream_name.into_inner();

    // Get the stream info and check if it exists
    let mut stream_manager_guard = stream_manager.lock().unwrap();
    let stream_info = match stream_manager_guard.streams.get(&stream_name) {
        Some(info) => info.clone(),
        None => return HttpResponse::NotFound().body("Stream not found"),
    };

    // Check if stream is already active
    if stream_manager_guard
        .active_streams
        .contains_key(&stream_name)
    {
        return HttpResponse::Ok().body("Stream already active");
    }

    // Create output directory
    let output_dir = format!("./streams/{}", stream_info.id);
    fs::create_dir_all(&output_dir).unwrap_or(());

    // Create a new DASH to HLS converter
    let converter = match DashToHlsConverter::new(&output_dir, stream_info.clone(), 40, 4) {
        Ok(conv) => Arc::new(Mutex::new(conv)),
        Err(e) => {
            return HttpResponse::InternalServerError()
                .body(format!("Failed to create converter: {}", e));
        }
    };

    let converter_clone = Arc::clone(&converter);
    stream_manager_guard
        .active_streams
        .insert(stream_name, converter);

    // Spawn a thread to run the converter
    thread::spawn(move || {
        if let Err(e) = DashToHlsConverter::run_streaming_loop(converter_clone) {
            error!("Streaming loop error: {}", e);
        }
    });

    HttpResponse::Ok().body("Stream initialization started")
}

async fn list_channels(
    _user: auth::AuthenticatedUser,
    stream_manager: web::Data<Arc<Mutex<StreamManager>>>,
) -> impl Responder {
    let stream_manager = stream_manager.lock().unwrap();
    let channels: Vec<ChannelInfo> = stream_manager
        .streams
        .values()
        .map(|info| ChannelInfo {
            id: info.id.clone(),
            name: info.name.clone(),
        })
        .collect();

    HttpResponse::Ok().json(channels)
}

async fn stream_status(
    _user: auth::AuthenticatedUser,
    stream_manager: web::Data<Arc<Mutex<StreamManager>>>,
) -> impl Responder {
    let stream_manager = stream_manager.lock().unwrap();

    let active_streams: Vec<String> = stream_manager.active_streams.keys().cloned().collect();

    HttpResponse::Ok().json(active_streams)
}

async fn stream_details(
    _user: auth::AuthenticatedUser,
    path: web::Path<String>,
    stream_manager: web::Data<Arc<Mutex<StreamManager>>>,
) -> impl Responder {
    let stream_id = path.into_inner();
    let stream_manager = stream_manager.lock().unwrap();

    if let Some(stream_info) = stream_manager.streams.get(&stream_id) {
        let is_active = stream_manager.active_streams.contains_key(&stream_id);

        let details = serde_json::json!({
            "id": stream_info.id,
            "name": stream_info.name,
            "active": is_active,
            "url": format!("/streams/{}/master.m3u8", stream_info.id),
        });

        HttpResponse::Ok().json(details)
    } else {
        HttpResponse::NotFound().body("Stream not found")
    }
}

fn start_cleanup_thread(
    secs: u64,
    stream_manager: &Arc<Mutex<StreamManager>>,
) -> anyhow::Result<()> {
    let stream_manager_clone = Arc::clone(&stream_manager);
    let timeout = Duration::from_secs(secs);

    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(15));

            let mut manager = stream_manager_clone.lock().unwrap();
            let now = Instant::now();
            let mut to_remove = vec![];

            for (stream_id, last_time) in manager.last_access.iter() {
                if now.duration_since(*last_time) > timeout {
                    to_remove.push(stream_id.clone());
                }
            }

            for stream_id in to_remove {
                if let Some(dashhlsconverter) = manager.active_streams.get(&stream_id) {
                    if let Ok(mut locked) = dashhlsconverter.lock() {
                        info!("Shutting down idle stream: {}", stream_id);
                        if let Err(e) = locked.stop() {
                            error!("Could not stop ffmpeg process: {}", e);
                        }
                    }
                }
                manager.active_streams.remove(&stream_id);
                manager.last_access.remove(&stream_id);
                info!("Removing folder: {}", &format!("./streams/{}", stream_id));
                if let Err(e) = fs::remove_dir_all(&format!("./streams/{}", stream_id)) {
                    error!("Error deleting folder: streams/{}: {}", stream_id, e);
                }
            }
        }
    });

    Ok(())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    info!("Starting DASH to HLS converter service");

    // Load configuration
    let deserializer = Deserializer::new("channels.toml".to_string(), "users.toml".to_string());

    // Load channels
    let channels_config = match deserializer.load_channels() {
        Ok(channels) => channels,
        Err(e) => {
            error!("Error reading channels.toml: {}", e);
            panic!("Can't continue without channels!");
        }
    };

    // Initialize stream manager
    let stream_manager = Arc::new(Mutex::new(StreamManager {
        streams: {
            let mut map = HashMap::new();
            for channel in channels_config.channel {
                map.insert(channel.id.clone(), StreamInfo {
                    id: channel.id,
                    name: channel.name,
                    url: channel.url,
                    key: channel.key,
                    init_segments: HashMap::new(),
                });
            }
            map
        },
        active_streams: HashMap::new(),
        last_access: HashMap::new(),
    }));

    // Load users
    let users_config = match deserializer.load_users() {
        Ok(users) => users,
        Err(e) => {
            error!("Error reading users.toml: {}", e);
            panic!("Can't continue without users!");
        }
    };

    // Initialize user manager
    let user_manager = Arc::new(Mutex::new(UserManager {
        users: {
            let mut map = HashMap::new();
            for user in users_config.user {
                map.insert(user.username, user.password);
            }
            map
        },
    }));

    // Create output directory
    fs::create_dir_all("./streams").unwrap_or(());

    // Printing local address to open link from localhost (the server actually listens from all
    // sources)
    info!("Starting server on http://127.0.0.1:8080");

    info!("Starting cleanup task");
    if let Err(e) = start_cleanup_thread(120, &stream_manager) {
        error!("Error starting cleanup task: {}", e);
    }

    // Start the web server
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(stream_manager.clone()))
            .app_data(web::Data::new(user_manager.clone()))
            .route("/login", web::post().to(login))
            .route("/init/{stream_id}", web::get().to(initialize_stream))
            .route("/status", web::get().to(stream_status))
            .route("/details/{stream_id}", web::get().to(stream_details))
            .route("/channels", web::get().to(list_channels))
            .route(
                "/streams/{stream_id}/{file_path:.*}",
                web::get().to(proxy_stream),
            )
            .service(Files::new("/", "./static").index_file("index.html"))
    })
    .bind("[::]:8080")?
    .workers(4)
    .run()
    .await
}
