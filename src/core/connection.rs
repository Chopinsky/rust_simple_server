use std::collections::HashMap;
use std::io::prelude::*;
use std::io::BufWriter;
use std::net::{TcpStream, Shutdown};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use core::config::ConnMetadata;
use core::http::{Request, Response, ResponseStates, ResponseWriter};
use core::router::{REST, Route, RouteHandler};

#[derive(PartialEq, Eq, Clone, Copy)]
enum ParseError {
    EmptyRequestErr,
    ReadStreamErr,
}

struct RequestBase {
    method: Option<REST>,
    uri: String,
    http_version: String,
    scheme: HashMap<String, Vec<String>>,
}

pub fn handle_connection(
        stream: TcpStream,
        router: Arc<Route>,
        conn_handler: Arc<ConnMetadata>
    ) -> Option<u8> {

    let request: Request;
    match read_request(&stream) {
        Ok(req) => {
            request = req;
        },
        Err(ParseError::ReadStreamErr) => {
            //can't read from the stream, no need to write back...
            stream.shutdown(Shutdown::Both).unwrap();
            return None;
        },
        Err(ParseError::EmptyRequestErr) => {
            println!("Error on parsing request");
            return write_to_stream(stream,
                                   build_default_response(&conn_handler.get_default_pages()),
                                   false);
        },
    }

    match handle_request_with_fallback(&request, &router,
                                       &conn_handler.get_default_header(),
                                       &conn_handler.get_default_pages()) {
        Ok(response) => {
            let ignore_body =
                match request.method {
                    Some(REST::OTHER(other_method)) => other_method.eq("head"),
                    _ => false,
                };

            return write_to_stream(stream, response, ignore_body);
        },
        Err(e) => {
            println!("Error on generating response -- {}", e);
            return write_to_stream(stream,
                                   build_default_response(&conn_handler.get_default_pages()),
                                   false);
        },
    }
}

fn write_to_stream(stream: TcpStream, response: Response, ignore_body: bool) -> Option<u8> {
    let mut buffer = BufWriter::new(stream);

    response.serialize_header(&mut buffer, ignore_body);
    if !ignore_body { response.serialize_body(&mut buffer); }

    if let Err(e) = buffer.flush() {
        println!("An error has taken place when flushing the response to the stream: {}", e);
        return Some(1);
    }

    if !response.to_close_connection() {
        return Some(0);
    } else {
        if let Ok(s) = buffer.into_inner() {
            if let Ok(_) = s.shutdown(Shutdown::Both) {
                return Some(0);
            }
        }
    }

    return Some(1);
}

fn read_request(mut stream: &TcpStream) -> Result<Request, ParseError> {
    let mut buffer = [0; 512];
    let result: Result<Request, ParseError>;

    if let Ok(_) = stream.read(&mut buffer) {
        let request = String::from_utf8_lossy(&buffer[..]);
        if request.is_empty() {
            return Err(ParseError::EmptyRequestErr);
        }

        result = match parse_request(&request) {
            Some(request_info) => Ok(request_info),
            None => Err(ParseError::EmptyRequestErr),
        };
    } else {
        result = Err(ParseError::ReadStreamErr);
    }

    result
}

fn parse_request(request: &str) -> Option<Request> {
    if request.is_empty() {
        return None;
    }

    //println!("{}", request);

    let mut method = None;
    let mut uri = String::new();
    let mut scheme = HashMap::new();
    let mut cookie = HashMap::new();
    let mut header = HashMap::new();

    let mut body = Vec::new();
    let mut is_body = false;

    let (tx_base, rx_base) = mpsc::channel();
    let (tx_cookie, rx_cookie) = mpsc::channel();

    for (num, line) in request.trim().lines().enumerate() {
        if num == 0 {
            if line.is_empty() { continue; }

            let val = line.to_owned();
            let tx_clone = mpsc::Sender::clone(&tx_base);

            thread::spawn(move || {
                parse_request_base(val, tx_clone);
            });

        } else {
            if line.is_empty() {
                // meeting the empty line dividing header and body
                is_body = true;
                continue;
            }

            if !is_body {
                let val = line.to_owned();
                let header_info: Vec<&str> = val.trim().splitn(2, ':').collect();

                if header_info.len() == 2 {
                    if header_info[0].trim().to_lowercase().eq("cookie") {
                        let cookie_body = header_info[1].to_owned();
                        let tx_clone = mpsc::Sender::clone(&tx_cookie);

                        thread::spawn(move || {
                            cookie_parser(cookie_body, tx_clone);
                        });

                    } else {
                        header.insert(
                            String::from(header_info[0].trim().to_lowercase()),
                            String::from(header_info[1].trim())
                        );
                    }
                }
            } else {
                body.push(line.to_owned());
                body.push(String::from("\r\n"));  //keep the line break
            }
        }
    }

    /* Since we don't move the tx but cloned them, need to drop them
     * specifically here, or we would hang forever before getting the
     * messages back.
     */
    drop(tx_base);
    drop(tx_cookie);

    if let Ok(base) = rx_base.recv_timeout(Duration::from_millis(200)) {
        method = base.method;
        uri = base.uri;
        scheme = base.scheme;
        header.entry(String::from("http_version")).or_insert(base.http_version);
    }

    if let Ok(cookie_set) = rx_cookie.recv_timeout(Duration::from_millis(200)) {
        cookie = cookie_set;
    }

    Some(Request::build_from(method, uri, scheme, cookie, header, body))
}

fn parse_request_base(line: String, tx: mpsc::Sender<RequestBase>) {
    let mut method = None;
    let mut uri = String::new();
    let mut http_version = String::new();
    let mut scheme = HashMap::new();

    for (index, info) in line.split_whitespace().enumerate() {
        match index {
            0 => {
                method = match &info[..] {
                    "GET" => Some(REST::GET),
                    "PUT" => Some(REST::PUT),
                    "POST" => Some(REST::POST),
                    "DELETE" => Some(REST::DELETE),
                    "OPTIONS" => Some(REST::OPTIONS),
                    "" => None,
                    _ => Some(REST::OTHER(info.to_lowercase().to_owned())),
                };
            },
            1 => {
                let (req_uri, req_scheme) = split_path(info);
                uri = req_uri.to_owned();

                if !req_scheme.is_empty() {
                    scheme_parser(&req_scheme[..], &mut scheme);
                }
            },
            2 => {
                http_version.push_str(info);
            },
            _ => { break; },
        };
    }

    if let Err(e) = tx.send(RequestBase {
        method,
        uri,
        http_version,
        scheme,
    }) {
        println!("Unable to parse base request: {}", e);
    }
}

fn handle_request_with_fallback(
        request_info: &Request,
        router: &Route,
        header: &HashMap<String, String>,
        fallback: &HashMap<u16, String>
    ) -> Result<Response, String> {

    let mut resp =
        if header.is_empty() {
            Response::new()
        } else {
            Response::new_with_default_header(&header)
        };

    match request_info.method {
        None => {
            return Err(String::from("Invalid request method"));
        },
        _ => {
            router.handle_request_method(&request_info, &mut resp);
        }
    }

    resp.check_and_update(&fallback);
    Ok(resp)
}

fn split_path(full_uri: &str) -> (String, String) {
    let uri = full_uri.trim();
    if uri.is_empty() {
        return (String::from("/"), String::new());
    }

    let mut uri_parts: Vec<&str> = uri.trim().rsplitn(2, "/").collect();

    if let Some(pos) = uri_parts[0].find("?") {
        let (last_uri_pc, scheme) = uri_parts[0].split_at(pos);
        uri_parts[0] = last_uri_pc;

        let real_uri =
            if uri_parts[1].is_empty() {
                format!("/{}", uri_parts[0])
            } else {
                format!("{}/{}", uri_parts[1], uri_parts[0])
            };

        (real_uri, scheme.trim().to_owned())
    } else {
        let uri_len = uri.len();
        let result_uri =
            if uri_len > 1 && uri.ends_with("/") {
                uri[..uri_len-1].to_owned()
            } else {
                uri.to_owned()
            };

        (result_uri, String::new())
    }
}

// Cookie parser will parse the request header's cookie field into a hash-map, where the
// field is the key of the map, which map to a single value of the key from the Cookie
// header field. Assuming no duplicate cookie keys, or the first cookie key-value pair
// will be stored.
fn cookie_parser(cookie_body: String, tx: mpsc::Sender<HashMap<String, String>>) { //cookie: &mut HashMap<String, String>) {
    if cookie_body.is_empty() { return; }

    let mut cookie = HashMap::new();
    for set in cookie_body.trim().split(";").into_iter() {
        let pair: Vec<&str> = set.trim().splitn(2, "=").collect();
        if pair.len() == 2 {
            cookie.entry(pair[0].trim().to_owned())
                .or_insert(pair[1].trim().to_owned());
        } else if pair.len() > 0 {
            cookie.entry(pair[0].trim().to_owned())
                .or_insert(String::new());
        }
    }

    if let Err(e) = tx.send(cookie) {
        println!("Unable to parse base request cookies: {}", e);
    }
}

fn scheme_parser(scheme: &str, scheme_collection: &mut HashMap<String, Vec<String>>) {
    for (_, kv_pair) in scheme.trim().split("&").enumerate() {
        let store: Vec<&str> = kv_pair.trim().splitn(2, "=").collect();
        if store.len() > 0 {
            let key = store[0].trim();
            let val =
                if store.len() == 2 {
                    store[1].trim().to_owned()
                } else {
                    String::new()
                };

            if scheme_collection.contains_key(key) {
                if let Some(val_vec) = scheme_collection.get_mut(key) {
                    val_vec.push(val);
                }
            } else {
                scheme_collection.insert(key.to_owned(), vec![val]);
            }
        }
    }
}

fn build_default_response(default_pages: &HashMap<u16, String>) -> Response {
    let mut resp = Response::new();

    resp.status(500);
    resp.check_and_update(&default_pages);

    resp
}
