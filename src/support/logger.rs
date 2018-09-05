#![allow(dead_code)]

use channel;
use chrono::{DateTime, Utc};
use std::env;
use std::fs;
use std::fs::File;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::io::Write;
use std::sync::{atomic::{AtomicBool, Ordering}, Mutex, Once, RwLock, ONCE_INIT};
use std::thread;
use std::time::Duration;

lazy_static! {
    static ref TEMP_STORE: Mutex<Vec<LogInfo>> = Mutex::new(Vec::new());
    static ref CONFIG: RwLock<LoggerConfig> = RwLock::new(LoggerConfig::initialize(""));
}

static DEFAULT_LOCATION: &'static str = "./logs";
static ONCE: Once = ONCE_INIT;
static mut SENDER: Option<channel::Sender<LogInfo>> = None;
static mut REFRESH_HANDLER: Option<thread::JoinHandle<()>> = None;
static mut DUMPING_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum InfoLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Critical,
}

impl fmt::Display for InfoLevel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

struct LogInfo {
    message: String,
    client: Option<SocketAddr>,
    level: InfoLevel,
    time: DateTime<Utc>,
}

struct LoggerConfig {
    id: String,
    refresh_period: Duration,
    log_folder_path: Option<PathBuf>,
    meta_info_provider: Option<fn(bool) -> String>,
    rx_handler: Option<thread::JoinHandle<()>>
}

impl LoggerConfig {
    fn initialize(id: &str) -> Self {
        LoggerConfig {
            id: id.to_owned(),
            refresh_period: Duration::from_secs(1800),
            log_folder_path: None,
            meta_info_provider: None,
            rx_handler: None,
        }
    }

    #[inline]
    fn set_id(&mut self, id: &str) {
        self.id = id.to_owned();
    }

    #[inline]
    fn get_id(&self) -> String {
        self.id.clone()
    }

    #[inline]
    fn get_refresh_period(&self) -> Duration {
        self.refresh_period.to_owned()
    }

    #[inline]
    fn set_refresh_period(&mut self, period: Duration) {
        self.refresh_period = period;
    }

    pub fn set_log_folder_path(&mut self, path: &str) {
        let mut path_buff = PathBuf::new();

        let location: Option<PathBuf> =
            if path.is_empty() {
                match env::var_os("LOG_FOLDER_PATH") {
                    Some(p) => {
                        path_buff.push(p);
                        Some(path_buff)
                    },
                    None => None,
                }
            } else {
                path_buff.push(path);
                Some(path_buff)
            };

        if let Some(loc) = location {
            if loc.as_path().is_dir() {
                self.log_folder_path = Some(loc);
            }
        }
    }

    #[inline]
    pub fn get_log_folder_path(&self) -> Option<PathBuf> {
        self.log_folder_path.clone()
    }
}

pub fn log(message: &str, level: InfoLevel, client: Option<SocketAddr>) -> Result<(), String> {
    let info = LogInfo {
        message: message.to_owned(),
        client,
        level,
        time: Utc::now(),
    };

    unsafe {
        if let Some(ref tx) = SENDER {
            tx.send(info);
            return Ok(());
        }
    }

    Err(String::from("The logging service is not running..."))
}

pub(crate) fn start(
    period: Option<u64>,
    log_folder_path: Option<&str>,
    meta_info_provider: Option<fn(bool) -> String>)
{
    if let Ok(mut config) = CONFIG.write() {
        if let Some(time) = period {
            if time != 1800 {
                config.refresh_period = Duration::from_secs(time);
            }
        }

        if let Some(path) = log_folder_path {
            config.set_log_folder_path(path);
        }
        
        config.meta_info_provider = meta_info_provider;
    }

    initialize();
}

pub(crate) fn shutdown() {
    stop_refresh();

    if let Ok(mut config) = CONFIG.write() {
        if let Some(rx) = config.rx_handler.take() {
            rx.join().unwrap_or_else(|err| {
                eprintln!("Encountered error while closing the logger: {:?}", err);
            });
        }
    }

    unsafe {
        let final_msg = LogInfo {
            message: String::from("Shutting down the logging service..."),
            client: None,
            level: InfoLevel::Info,
            time: Utc::now(),
        };

        if let Some(ref tx) = SENDER.take() {
            tx.send(final_msg);
        }
    }

    // Need to block because otherwise the lazy_static contents may go expired too soon
    dump_to_file();
}

fn initialize() {
    ONCE.call_once(|| {
        let (tx, rx): (channel::Sender<LogInfo>, channel::Receiver<LogInfo>) = channel::unbounded();

        unsafe { SENDER = Some(tx); }

        if let Ok(mut config) = CONFIG.write() {
            if let Some(ref path) = config.log_folder_path {
                let refresh = config.refresh_period.as_secs();

                println!(
                    "The logger has started, it will refresh log to folder {:?} every {} seconds",
                    path.to_str().unwrap(), refresh
                );

                start_refresh(config.refresh_period.clone());
            }

            config.rx_handler = Some(thread::spawn(move || {
                for info in rx {
                    if let Ok(mut store) = TEMP_STORE.lock() {
                        store.push(info);
                    }
                }
            }));
        }
    });
}

fn start_refresh(period: Duration) {
    unsafe {
        if REFRESH_HANDLER.is_some() {
            stop_refresh();
        }

        REFRESH_HANDLER = Some(thread::spawn(move ||
            loop {
                thread::sleep(period);
                thread::spawn(|| {
                    dump_to_file();
                });
            }
        ));
    }
}

fn stop_refresh() {
    unsafe {
        if let Some(handler) = REFRESH_HANDLER.take() {
            handler.join().unwrap_or_else(|err| {
                eprintln!(
                    "Failed to stop the log refresh service, error code: {:?}...",
                    err
                );
            });
        }
    }
}

fn reset_refresh(period: Option<Duration>) {
    thread::spawn(move || {
        stop_refresh();

        let new_period =
            if let Ok(mut config) = CONFIG.write() {
                if let Some(p) = period {
                    config.set_refresh_period(p);
                }

                config.get_refresh_period()

            } else {

                Duration::from_secs(1800)
            };

        start_refresh(new_period);
    });
}

fn dump_to_file() {
    unsafe {
        if let Ok(config) = CONFIG.read() {
            let file = match config.log_folder_path {
                Some(ref location) if location.is_dir() => {
                    create_dump_file(config.get_id(), location)
                },
                _ => {
                    create_dump_file(config.get_id(), &PathBuf::from(DEFAULT_LOCATION))
                },
            };

            if let Ok(mut file) = file {
                if DUMPING_RUNNING.load(Ordering::Relaxed) {
                    if let Some(meta_func) = config.meta_info_provider {
                        write_to_file(&mut file, meta_func(false));
                    }

                    write_to_file(&mut file,
                                  format_content(InfoLevel::Info, "A dumping process is already in progress, skipping this scheduled dump."));
                    return;
                }

                // Now start a new dump
                *DUMPING_RUNNING.get_mut() = true;

                //TODO: writing to file
                if let Ok(mut store) = TEMP_STORE.lock() {
                    let mut content: String;
                    while let Some(info) = store.pop() {
                        content = format!("{}", info.message);
                    }
                }

                *DUMPING_RUNNING.get_mut() = false;
            }
        }
    }
}

fn write_to_file(file: &mut File, content: String) {
    file.write_all(content.as_bytes()).unwrap_or_else(|err| {
        eprintln!("Failed to write to dump file: {}...", err);
    });
}

fn format_content(level: InfoLevel, message: &str) -> String {
    format!("\n[{}] @ {}: {}", level.to_string(), Utc::now().to_rfc3339(), message)
}

fn create_dump_file(id: String, loc: &PathBuf) -> Result<File, String> {
    if !loc.as_path().is_dir() {
        if let Err(e) = fs::create_dir_all(loc) {
            return Err(format!("Failed to dump the logging information: {}", e));
        }
    }

    let base: String =
        if id.is_empty() {
            format!("{}.txt", Utc::now().to_string())
        } else {
            format!("{}-{}.txt", id, Utc::now().to_string())
        };

    let mut path = loc.to_owned();
    path.push(base);

    match File::create(path.as_path()) {
        Ok(file) => Ok(file),
        _ => Err(String::from("Unable to create the dump file")),
    }
}
