#![allow(unused_imports)]
#![allow(unused_variables)]

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::Error;
use std::sync::mpsc;
use std::time::Duration;

use super::http::{Request, RequestWriter, Response, ResponseStates, ResponseWriter};
use regex::Regex;
use support::common::MapUpdates;
use support::debug;
use support::Field;
use support::TaskType;
use support::{shared_pool, RouteTrie};

lazy_static! {
    static ref ROUTE_ALL: REST = REST::OTHER(String::from("*"));
}

#[derive(PartialEq, Eq, Hash, Clone)]
pub enum REST {
    GET,
    PATCH,
    POST,
    PUT,
    DELETE,
    OPTIONS,
    OTHER(String),
}

#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub enum RequestPath {
    Explicit(&'static str),
    ExplicitWithParams(&'static str),
    WildCard(&'static str),
}

/// `Callback` is a type alias to the REST request handler functions, which will be invoked when a
/// client request has been received on the associated URI or pattern.
pub type Callback = fn(&Box<Request>, &mut Box<Response>);

/// `AuthFunc` is a type alias to the authentication functions, which is optional, but if set, it
/// will be invoked right after we parse the client request to determine if the requested URI is
/// allowed to be visited by the client: if denied, we will generate the 403 error message as the
/// respones. This function is generally to be used as the gate-keeper, e.g. if a use is logged in
/// to see the dashboard routes.
///
/// The function takes 2 input parameters: 1) request: &Box<Request>, which contains all information
/// from the client request; 2) the URI from the request: String, which is the URI being requested,
/// this information is also available from the `request` parameter, but we extracted out to make it
/// easier to access.
///
/// The function takes 1 boolean output, where `true` means the access is allowed, and `false` means
/// the access is denied.
///
/// The use of the AuthFunc is totally optional, you can also check authentication within individual
/// request handlers as well. You can also use the `context` and/or `session` modules to store, or
/// update persistent information regarding the client requestor.
pub type AuthFunc = fn(&Box<Request>, String) -> bool;

struct RegexRoute {
    pub regex: Regex,
    pub handler: Callback,
}

impl RegexRoute {
    pub fn new(re: Regex, handler: Callback) -> Self {
        RegexRoute { regex: re, handler }
    }
}

impl Clone for RegexRoute {
    fn clone(&self) -> Self {
        RegexRoute {
            regex: self.regex.clone(),
            handler: self.handler,
        }
    }
}

pub struct RouteMap {
    explicit: HashMap<String, Callback>,
    explicit_with_params: RouteTrie,
    wildcard: HashMap<String, RegexRoute>,
}

impl RouteMap {
    pub fn new() -> Self {
        RouteMap {
            explicit: HashMap::new(),
            explicit_with_params: RouteTrie::initialize(),
            wildcard: HashMap::new(),
        }
    }

    pub fn insert(&mut self, uri: RequestPath, callback: Callback) {
        match uri {
            RequestPath::Explicit(req_uri) => {
                if req_uri.is_empty() || !req_uri.starts_with('/') {
                    panic!("Request path must have valid contents and start with '/'.");
                }

                self.explicit.add(req_uri, callback, false);
            },
            RequestPath::WildCard(req_uri) => {
                if req_uri.is_empty() {
                    panic!("Request path must have valid contents.");
                }

                if self.wildcard.contains_key(req_uri) {
                    return;
                }

                if let Ok(re) = Regex::new(req_uri) {
                    self.wildcard
                        .add(req_uri, RegexRoute::new(re, callback), false);
                }
            },
            RequestPath::ExplicitWithParams(req_uri) => {
                if !req_uri.contains("/:") && !req_uri.contains(":\\") {
                    self.explicit.add(req_uri, callback, false);
                    return;
                }

                self.explicit_with_params.add(RouteMap::params_parser(req_uri), callback);
            },
        }
    }

    fn params_parser(source_uri: &'static str) -> Vec<Field> {
        let mut param_names = HashSet::new();

        let mut validation: Option<Regex> = None;
        let mut name = "";
        let mut is_param = false;

        // Status: 0 -- Normal; 1 -- Just split; 2 -- In params; 4 -- In params regex;
        //         8 -- Params regex just end, must split next or panic.
        let mut split_status: u8 = 0;

        source_uri
            .split(|c|
                // split status Automator
                match c {
                    ':' if split_status == 1 => {
                        split_status <<= 1;  // 2 -- in params
                        false
                    },
                    '(' if split_status == 2 => {
                        split_status <<= 1; // 4 -- in params regex
                        false
                    },
                    ')' if split_status == 4 => {
                        split_status <<= 1; // 8 -- in params regex end
                        false
                    },
                    '/' if split_status == 0 || split_status == 2 || split_status == 8 => {
                        split_status = 1;   // reset to 1 -- just split
                        true
                    },
                    '/' if split_status == 1 => {
                        panic!("Route can't contain empty segment between '/'s: {}", source_uri);
                    },
                    _ => {
                        if split_status == 2 && !char::is_alphanumeric(c) {
                            panic!("Route's parameter name can only contain alpha-numeric characters: {}", source_uri);
                        }

                        if split_status == 8 {
                            panic!("Route's parameter with regex validation must end after the regex: {}", source_uri);
                        }

                        if split_status == 1 {
                            split_status >>= 1; // does not encounter special flags, this is an explicit uri segment name
                        }

                        false
                    },
                }
            )
            .filter_map(|s| {
                if s.is_empty() {
                    return None;
                }

                validation = None;
                is_param = false;

                if s.starts_with(':') {
                    name = &s[1..];

                    if name.is_empty() {
                        panic!("Route parameter name can't be null");
                    }

                    is_param = true;
                    if name.len() > 1 && name.ends_with(')') {
                        let name_split: Vec<&str> =
                            (&name[..name.len()-1]).splitn(2, '(').collect();

                        if name_split.len() == 2 {
                            if name_split[0].is_empty() {
                                panic!("Route parameters with regex validation must have a non-null param name: {}", s);
                            } else if name_split[1].is_empty() {
                                panic!("Route parameters with regex validation must have a non-null regex: {}", s);
                            }

                            if let Ok(regex) = Regex::new(name_split[1]) {
                                validation = Some(regex);
                                name = name_split[0];
                            }
                        }
                    }

                    if param_names.contains(name) {
                        panic!("Route parameters must have unique name: {}", s);
                    }

                    param_names.insert(name.to_owned());
                } else {
                    name = &s;
                }

                Some(Field::new(name.to_owned(), is_param, validation.take()))
            })
            .collect()
    }

    fn seek_path(&self, uri: &str, params: &mut HashMap<String, String>) -> Option<Callback> {
        if let Some(callback) = self.explicit.get(uri) {
            return Some(*callback);
        }

        if !self.explicit_with_params.is_empty() {
            let (callback, temp_params) =
                search_params_router(&self.explicit_with_params, uri);

            if callback.is_some() {
                for param in temp_params {
                    params.insert(param.0, param.1);
                }

                return callback;
            }
        }

        if !self.wildcard.is_empty() {
            return search_wildcard_router(&self.wildcard, uri);
        }

        None
    }
}

impl Clone for RouteMap {
    fn clone(&self) -> Self {
        RouteMap {
            explicit: self.explicit.clone(),
            explicit_with_params: self.explicit_with_params.clone(),
            wildcard: self.wildcard.clone(),
        }
    }
}

pub struct Route {
    store: Box<HashMap<REST, RouteMap>>,
    auth_func: Option<AuthFunc>,
}

impl Route {
    pub fn new() -> Self {
        Route {
            store: Box::from(HashMap::new()),
            auth_func: None,
        }
    }

    #[inline]
    pub fn get_auth_func(&self) -> Option<AuthFunc> {
        self.auth_func.clone()
    }

    #[inline]
    pub fn set_auth_func(&mut self, auth_func: Option<AuthFunc>) {
        self.auth_func = auth_func;
    }

    fn add_route(&mut self, method: REST, uri: RequestPath, callback: Callback) {
        if let Some(route) = self.store.get_mut(&method) {
            //find, insert, done.
            route.insert(uri, callback);
            return;
        }

        // the route for the given method has not yet initialized
        let mut route = RouteMap::new();
        route.insert(uri, callback);

        self.store.insert(method, route);
    }
}

impl Clone for Route {
    fn clone(&self) -> Self {
        Route {
            store: self.store.clone(),
            auth_func: self.auth_func.clone(),
        }
    }
}

pub trait Router {
    fn get(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
    fn patch(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
    fn post(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
    fn put(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
    fn delete(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
    fn options(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
    fn other(&mut self, method: &str, uri: RequestPath, callback: Callback) -> &mut Route;
    fn all(&mut self, uri: RequestPath, callback: Callback) -> &mut Route;
}

impl Router for Route {
    fn get(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.add_route(REST::GET, uri, callback);
        self
    }

    fn patch(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.add_route(REST::PATCH, uri, callback);
        self
    }

    fn post(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.add_route(REST::POST, uri, callback);
        self
    }

    fn put(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.add_route(REST::PUT, uri, callback);
        self
    }

    fn delete(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.add_route(REST::DELETE, uri, callback);
        self
    }

    fn options(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.add_route(REST::OPTIONS, uri, callback);
        self
    }

    fn other(&mut self, method: &str, uri: RequestPath, callback: Callback) -> &mut Route {
        if method.is_empty() {
            panic!("Must provide a valid method!");
        }

        let request_method = REST::OTHER(method.to_uppercase());
        self.add_route(request_method, uri, callback);

        self
    }

    /// Function 'all' will match the uri on all request methods. Note that the "match all" paradigm
    /// is used in this framework as a safe fallback, which means that if a different callback
    /// has been defined for the same uri but under a explicitly defined request method (e.g. get,
    /// post, etc.), it will be matched and invoked instead of the "match all" callback functions.
    fn all(&mut self, uri: RequestPath, callback: Callback) -> &mut Route {
        self.other("*", uri.clone(), callback)
    }
}

pub trait RouteHandler {
    fn handle_request(callback: Callback, req: &Box<Request>, resp: &mut Box<Response>);
    fn seek_handler(
        &self,
        method: &REST,
        uri: &str,
        header_only: bool,
        tx: mpsc::Sender<(Option<Callback>, HashMap<String, String>)>,
    );
}

impl RouteHandler for Route {
    fn handle_request(callback: Callback, req: &Box<Request>, resp: &mut Box<Response>) {
        // callback function will decide what to be written into the response
        callback(req, resp);

        // if a redirect response, set up as so.
        let mut redirect = resp.get_redirect_path();
        if !redirect.is_empty() {
            if !redirect.starts_with('/') {
                redirect.insert(0, '/');
            }

            resp.header("Location", &redirect, true);
            resp.status(301);
        }
    }

    fn seek_handler(
        &self,
        method: &REST,
        uri: &str,
        header_only: bool,
        tx: mpsc::Sender<(Option<Callback>, HashMap<String, String>)>,
    ) {
        let mut result = None;
        let mut params = HashMap::new();

        if let Some(routes) = self.store.get(method) {
            result = routes.seek_path(uri, &mut params);

        } else if header_only {
            //if a header only request, fallback to search with REST::GET
            if let Some(routes) = self.store.get(&REST::GET) {
                result = routes.seek_path(uri, &mut params);
            }

        }

        if result.is_none() {
            if let Some(all_routes) = self.store.get(&ROUTE_ALL) {
                result = all_routes.seek_path(uri, &mut params);
            }
        }

        if let Err(e) = tx.send((result, params)) {
            debug::print("Unable to find the route handler", 2);
        }
    }
}

fn search_wildcard_router(routes: &HashMap<String, RegexRoute>, uri: &str) -> Option<Callback> {
    let mut result = None;
    for (_, route) in routes.iter() {
        if route.regex.is_match(&uri) {
            result = Some(route.handler);
            break;
        }
    }

    result
}

fn search_params_router(
    route_head: &RouteTrie,
    uri: &str,
) -> (Option<Callback>, Vec<(String, String)>) {

    let raw_segments: Vec<String> =
        uri.trim_matches('/')
            .split('/')
            .map(|s| s.to_owned())
            .collect();

    let mut params: Vec<(String, String)> = Vec::new();
    let result =
        RouteTrie::find(route_head, raw_segments.as_slice(), &mut params);

    (result, params)
}

#[cfg(test)]
mod route_test {
    use super::{Field, RouteMap};
    use regex::*;

    #[test]
    fn params_parser_test_one() {
        let regex = Regex::new("a=[/]bdc").unwrap();
        let base = vec![
            Field::new(String::from("root"), false, None),
            Field::new(String::from("api"), false, None),
            Field::new(String::from("Tes中t"), true, Some(regex)),
            Field::new(String::from("this."), false, None),
            Field::new(String::from("check"), true, None),
        ];

        let test = RouteMap::params_parser("/root/api/:Tes中t(a=[/]bdc)/this./:check/");
        assert_eq!(test.len(), base.len());

        for (base_field, test_field) in base.iter().zip(&test) {
            assert_eq!(base_field, test_field);
        }
    }
}