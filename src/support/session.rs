
use std::collections::HashMap;
use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::ops::*;
use std::path::Path;
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, SystemTime};
use std::thread;
use std::thread::*;

use chrono;
use chrono::prelude::*;
use rand::{thread_rng, Rng};
use support::ThreadPool;

static DELEM_LV_1: char = '\u{0005}';
static DELEM_LV_2: char = '\u{0006}';
static DELEM_LV_3: char = '\u{0007}';
static DELEM_LV_4: char = '\u{0008}';

lazy_static! {
    static ref STORE: Arc<RwLock<HashMap<String, Session>>> = Arc::new(RwLock::new(HashMap::new()));
    static ref DEFAULT_LIFETIME: Arc<RwLock<Duration>> = Arc::new(RwLock::new(Duration::from_secs(172800)));
    static ref AUTO_CLEARN: Arc<RwLock<bool>> = Arc::new(RwLock::new(false));
}

pub struct Session {
    id: String,
    expires_at: chrono::DateTime<Utc>,
    auto_renewal: bool,
    store: HashMap<String, String>,
}

impl Session {
    pub fn clone(&self) -> Self {
        Session {
            id: self.id.to_owned(),
            expires_at: self.expires_at.clone(),
            auto_renewal: self.auto_renewal,
            store: self.store.clone(),
        }
    }

    pub fn to_owned(&self) -> Self {
        Session {
            id: self.id.to_owned(),
            expires_at: self.expires_at.to_owned(),
            auto_renewal: self.auto_renewal,
            store: self.store.to_owned(),
        }
    }

    fn serialize(&self) -> String {
        let mut result = String::new();
        if self.id.is_empty() { return result; }

        let expires_at = self.expires_at.to_rfc3339();
        result.push_str(&format!("{}{}", self.id.to_owned(), DELEM_LV_2));
        result.push_str(&format!("{}{}", expires_at, DELEM_LV_2));
        result.push_str(&format!("{}{}", self.auto_renewal.to_string(), DELEM_LV_2));

        for (key, val) in self.store.iter() {
            let entry = format!("{}{}{}", *key, DELEM_LV_4, *val);
            result.push_str(&format!("{}{}", entry, DELEM_LV_3));
        }

        result
    }

    fn deserialize(raw: &str, default_expires: DateTime<Utc>, now: chrono::DateTime<Utc>) -> Option<Session> {
        if raw.is_empty() { return None; }

        let mut id = String::new();
        let mut expires_at = default_expires.clone();
        let mut auto_renewal = false;
        let mut store = HashMap::new();

        for (index, field) in raw.trim().split(DELEM_LV_2).enumerate() {
            match index {
                0 => {
                    id = field.to_owned();
                    if id.is_empty() { return None; }
                },
                1 => {
                    if let Ok(parsed_expiration) = field.parse::<DateTime<Utc>>() {
                        expires_at = parsed_expiration;
                        if expires_at.cmp(&now) == Ordering::Less {
                            //already expired, return null
                            return None;
                        }
                    }
                },
                2 => if field.eq("true") { auto_renewal = true; },
                3 => parse_session_store(&mut store, field),
                _ => { break; },
            }
        }

        return Some(Session {
            id,
            expires_at,
            auto_renewal,
            store,
        });
    }
}

pub trait SessionExchange {
    fn initialize_new() -> Option<Session>;
    fn initialize_new_with_id(id: &str) -> Option<Session>;
    fn from_id(id: String) -> Option<Session>;
    fn from_or_new(id: String) -> Option<Session>;
    fn release(id: String);
    fn set_default_session_lifetime(lifetime: Duration);
    fn clean();
    fn clean_up_to(lifetime: DateTime<Utc>);
    fn store_size() -> Option<usize>;
    fn auto_clean_start(period: Duration) -> Thread;
    fn auto_clean_has_stopped();
    fn auto_clean_is_running() -> bool;
}

impl SessionExchange for Session {
    fn initialize_new() -> Option<Self> {
        new_session("")
    }

    fn initialize_new_with_id(id: &str) -> Option<Self> {
        new_session(id)
    }

    fn from_id(id: String) -> Option<Self> {
        if let Ok(store) = STORE.read() {
            if let Some(val) = store.get(&id) {
                if val.expires_at.cmp(&Utc::now()) != Ordering::Less {
                    //found the session, return now
                    return Some(val.to_owned());

                } else {
                    //expired, remove it from the store
                    thread::spawn(move || {
                        release(id);
                    });

                    return None;
                }
            }
        }

        None
    }

    fn from_or_new(id: String) -> Option<Self> {
        if let Some(session) = Session::from_id(id) {
            Some(session)
        } else {
            Session::initialize_new()
        }
    }

    fn release(id: String) {
        thread::spawn(move || {
            release(id);
        });
    }

    fn set_default_session_lifetime(lifetime: Duration) {
        thread::spawn(move || {
            if let Ok(mut default_lifetime) = DEFAULT_LIFETIME.write() {
                *default_lifetime = lifetime;
            }
        });
    }

    fn clean() {
        thread::spawn(move || {
            clean_up_to(Utc::now());
        });
    }

    fn clean_up_to(lifetime: DateTime<Utc>) {
        let now = Utc::now();
        let time =
            if lifetime.cmp(&now) != Ordering::Greater {
                now
            } else {
                lifetime
            };

        thread::spawn(move || {
            clean_up_to(time);
        });
    }

    fn store_size() -> Option<usize> {
        if let Ok(store) = STORE.read() {
            Some(store.keys().len())
        } else {
            None
        }
    }

    fn auto_clean_start(period: Duration) -> Thread {
        let sleep_period =
            if period.cmp(&Duration::from_secs(60)) == Ordering::Less {
                Duration::from_secs(60)
            } else {
                period
            };

        let handler: JoinHandle<_> = thread::spawn(move || {
            if let Ok(mut auto_clean) = AUTO_CLEARN.write() {
                *auto_clean = true;
            }

            loop {
                thread::sleep(sleep_period);
                clean_up_to(Utc::now());
            }
        });

        handler.thread().to_owned()
    }

    fn auto_clean_has_stopped() {
        thread::spawn(move || {
            if let Ok(mut auto_clean) = AUTO_CLEARN.write() {
                *auto_clean = false;
            }
        });
    }

    fn auto_clean_is_running() -> bool {
        if let Ok(auto_clean) = AUTO_CLEARN.read() {
            return *auto_clean;
        }

        return false;

    }
}

pub trait SessionHandler {
    fn get_id(&self) -> String;
    fn get_value(&self, key: &str) -> Option<String>;
    fn set_value(&mut self, key: &str, val: &str) -> Option<String>;
    fn auto_lifetime_renew(&mut self, auto_renewal: bool);
    fn expires_at(&mut self, expires_at: DateTime<Utc>);
    fn save(&mut self);
}

impl SessionHandler for Session {
    fn get_id(&self) -> String {
        self.id.to_owned()
    }

    fn get_value(&self, key: &str) -> Option<String> {
        if let Some(val) = self.store.get(key) {
            Some(val.to_owned())
        } else {
            None
        }
    }

    // Set new session key-value pair, returns the old value if the key
    // already exists
    fn set_value(&mut self, key: &str, val: &str) -> Option<String> {
        self.store.insert(key.to_owned(), val.to_owned())
    }

    fn auto_lifetime_renew(&mut self, auto_renewal: bool) {
        self.auto_renewal = auto_renewal;
    }

    // Set the expires system time. This will turn off auto session life time
    // renew if it's set.
    fn expires_at(&mut self, expires_time: DateTime<Utc>) {
        if self.auto_renewal {
            self.auto_renewal = false;
        }

        self.expires_at = expires_time;
    }

    fn save(&mut self) {
        save(self.id.to_owned(), self);
    }
}

pub trait PersistHandler {
    fn init_from_file(path: &Path) -> bool;
    fn save_to_file(path: &Path);
}

impl PersistHandler for Session {

    //TODO:allow decreptor
    fn init_from_file(path: &Path) -> bool {
        let mut buf_reader =
            if let Ok(dest_file) = File::open(&path) {
                BufReader::new(dest_file)
            } else {
                // can't read the file, abort saving
                eprintln!("Unable to open the session store file, please check if the file exists.");
                return false;
            };

        let pool = ThreadPool::new(8);
        let (tx, rx): (mpsc::Sender<Option<Session>>, mpsc::Receiver<Option<Session>>) = mpsc::channel();

        let now = Utc::now();
        let default_expires = get_next_expiration();

        let mut failures: u8 = 0;
        loop {
            let mut buf: Vec<u8> = Vec::new();
            if let Ok(size) = buf_reader.read_until(DELEM_LV_1 as u8, &mut buf) {
                if size == 0 { break; }

                buf.pop();
                if let Ok(session) = String::from_utf8(buf) {
                    if session.is_empty() { continue; }

                    let tx_clone = mpsc::Sender::clone(&tx);
                    pool.execute(move || {
                        recreate_session_from_raw(session, &now, &default_expires, tx_clone);
                    });
                }
            } else {
                failures += 1;
                if failures > 5 {
                    break;
                }
            }
        }

        drop(tx);

        if let Ok(mut store) = STORE.write() {
            for received in rx {
                if let Some(session) = received {
                    let id: String = session.id.to_owned();
                    store.entry(id).or_insert(session);  //if a key collision, always keep the early entry.
                }
            }
        }

        true
    }

    //TODO:allow encryptor
    fn save_to_file(path: &Path) {
        let save_path = path.to_owned();
        let handler = thread::spawn(move || {
            let mut file =
                if let Ok(dest_file) = File::create(&save_path) {
                    dest_file
                } else {
                    // can't create file, abort saving
                    return;
                };

            if let Ok(store) = STORE.read() {
                let mut count: u8 = 0;
                for (_, val) in store.iter() {
                    let s = format!("{}{}", val.serialize(), DELEM_LV_1);

                    if s.is_empty() { continue; }
                    if let Err(_) = file.write(s.as_bytes()) { continue; }

                    count += 1;
                    if count % 32 == 0 {
                        if let Err(e) = file.flush() {
                            eprintln!("Failed to flush the session store to the file store: {}", e);
                        }

                        count = 0;
                    }
                }

                if let Err(e) = file.sync_all() {
                    eprintln!("Unable to sync all session data to the file store: {}", e);
                }
            }
        });

        //make sure saving is finished
        handler.join().unwrap();
    }
}

fn new_session(id: &str) -> Option<Session> {
    let next_id: String;
    if id.is_empty() {
        next_id = match gen_session_id(16) {
            Some(val) => val,
            None => String::new(),
        };

        if next_id.is_empty() { return None; }
    } else {
        next_id = id.to_owned();
    }

    let session = Session {
        id: next_id,
        expires_at: get_next_expiration(),
        auto_renewal: true,
        store: HashMap::new(),
    };

    if let Ok(mut store) = STORE.write() {
        //if key already exists, override to protect session scanning
        store.insert(session.id.to_owned(), session.to_owned());
        Some(session)
    } else {
        None
    }
}

fn gen_session_id(id_size: usize) -> Option<String> {
    let size =
        if id_size < 16 {
            16
        } else {
            id_size
        };

    let mut next_id: String =
        thread_rng().gen_ascii_chars().take(size).collect();

    if let Ok(store) = STORE.read() {
        let begin = SystemTime::now();
        let mut count = 1;

        loop {
            if !store.contains_key(&next_id) {
                return Some(next_id);
            }

            if count % 32 == 0 {
                count = 1;
                if SystemTime::now().sub(Duration::from_millis(256)) > begin {
                    // 256 milli-sec for get a good guess is already too expansive...
                    return None;
                }
            }

            // now take the next guess
            next_id = thread_rng().gen_ascii_chars().take(32).collect();
            count += 1;
        }
    }

    None
}

fn save(id: String, session: &mut Session) -> bool {
    if let Ok(mut store) = STORE.write() {
        if session.auto_renewal {
            session.expires_at = get_next_expiration();
        }

        let old_session = store.insert(id, session.to_owned());
        drop(old_session);

        true
    } else {
        false
    }
}

fn get_next_expiration() -> chrono::DateTime<Utc> {
    if let Ok(default_lifetime) = DEFAULT_LIFETIME.read() {
        if let Ok(life_time) = chrono::Duration::from_std(*default_lifetime) {
            return Utc::now().add(life_time);
        }
    }

    Utc::now().add(chrono::Duration::seconds(172800))
}

fn release(id: String) -> bool {
    if let Ok(mut store) = STORE.write() {
        store.remove(&id);
    } else {
        return false;
    }

    true
}

fn clean_up_to(time: DateTime<Utc>) {
    let mut stale_sessions: Vec<String> = Vec::new();
    if let Ok(mut store) = STORE.write() {
        for session in store.values() {
            if session.expires_at.cmp(&time) != Ordering::Greater {
                stale_sessions.push(session.id.to_owned());
            }
        }

        println!("Cleaned: {}", stale_sessions.len());

        for id in stale_sessions {
            store.remove(&id);
        }
    }

    println!("Session clean done!");
}

fn parse_session_store(store: &mut HashMap<String, String>, field: &str) {
    if field.is_empty() { return; }

    for (_, entry) in field.trim().split(DELEM_LV_3).enumerate() {
        if let Some(pos) = entry.find(DELEM_LV_4) {
            let (key, value): (&str, &str) = entry.split_at(pos);
            if !key.is_empty() {
                store.entry(key.to_owned()).or_insert(value.to_owned());
            }
        }
    }
}

fn recreate_session_from_raw(raw: String, now: &DateTime<Utc>, expires_at: &DateTime<Utc>, tx: mpsc::Sender<Option<Session>>) {
    let result =
        Session::deserialize(&raw[..], expires_at.to_owned(), now.to_owned());

    if let Err(e) = tx.send(result) {
        println!("Unable to parse base request: {:?}", e);
    }
}