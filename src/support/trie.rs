#![allow(dead_code)]

use crate::core::router::Callback;
use crate::hashbrown::HashMap;
use crate::regex::Regex;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct Field {
    name: String,
    is_param: bool,
    validation: Option<Regex>,
}

impl Field {
    pub(crate) fn new(name: String, is_param: bool, validation: Option<Regex>) -> Self {
        Field {
            name,
            is_param,
            validation,
        }
    }
}

impl Clone for Field {
    fn clone(&self) -> Self {
        Field {
            name: self.name.clone(),
            is_param: self.is_param,
            validation: self.validation.clone(),
        }
    }
}

impl PartialEq for Field {
    fn eq(&self, other: &Field) -> bool {
        if self.name != other.name
            || self.is_param != other.is_param
            || self.validation.is_some() != other.validation.is_some()
        {
            return false;
        }

        if let Some(ref reg_one) = self.validation {
            if let Some(ref reg_two) = other.validation {
                return reg_one.to_string() == reg_two.to_string();
            }
        }

        true
    }
}

struct Node {
    field: Field,
    callback: Option<Callback>,
    location: Option<PathBuf>,
    named_children: HashMap<String, Node>,
    params_children: Vec<Node>,
}

impl Node {
    fn new(field: Field, callback: Option<Callback>, location: Option<PathBuf>) -> Self {
        Node {
            field,
            callback,
            location,
            named_children: HashMap::new(),
            params_children: Vec::new(),
        }
    }

    fn insert(
        &mut self, mut segments: Vec<Field>, callback: Option<Callback>, location: Option<PathBuf>
    ) {
        debug_assert!(callback.is_some() || location.is_some());

        let head = match segments.pop() {
            Some(seg) => seg,
            None => return,
        };

        // if already has this child, keep calling insert recursively. Only do this when not
        // a params, otherwise, always create a new branch
        if !head.is_param {
            if let Some(child) = self.named_children.get_mut(&head.name) {
                if segments.len() == 0 {
                    // done, update the node
                    if (callback.is_some() && child.callback.is_some())
                        || (location.is_some() && child.location.is_some())
                    {
                        panic!("Key collision!");
                    }

                    if callback.is_some() {
                        child.callback = callback;
                    }

                    if location.is_some() {
                        child.location = location;
                    }
                } else {
                    // recursive insert to the child
                    child.insert(segments, callback, location);
                }

                return;
            }

            self.named_children.insert(
                head.name.clone(),
                Node::build_new_child(head, segments, callback, location),
            );

            return;
        }

        self.params_children
            .push(Node::build_new_child(head, segments, callback, location));
    }

    fn build_new_child(
        field: Field, segments: Vec<Field>, callback: Option<Callback>, loc: Option<PathBuf>
    ) -> Node
    {
        match segments.len() {
            0 => {
                // leaf node
                Node::new(field, callback, loc)
            },
            _ => {
                // branch node
                let mut node = Node::new(field, None, None);
                node.insert(segments, callback, loc);
                node
            }
        }
    }
}

impl Clone for Node {
    fn clone(&self) -> Self {
        Node {
            field: self.field.clone(),
            callback: self.callback,
            location: self.location.clone(),
            named_children: self.named_children.clone(),
            params_children: self.params_children.clone(),
        }
    }
}

pub(crate) struct RouteTrie {
    root: Node,
}

impl RouteTrie {
    pub(crate) fn initialize() -> Self {
        RouteTrie {
            root: Node::new(
                Field::new(String::from("/"), false, None),
                None,
                None
            ),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.root.named_children.is_empty() && self.root.params_children.is_empty()
    }

    pub(crate) fn add(&mut self, segments: Vec<Field>, callback: Callback) {
        self.root.insert(segments, Some(callback), None);
    }

    // global: location + path/to/file/in/uri
    pub(crate) fn add_global_static(&mut self, location: PathBuf) {
        self.root.location.replace(location);
    }

    /*pub(crate)*/ fn add_local_static(&mut self, segments: Vec<Field>, location: PathBuf) {
        //TODO: make the API public
        self.root.insert(segments, None, Some(location));
    }

    pub(crate) fn find(
        route_head: &RouteTrie,
        segments: &[String],
        params: &mut Vec<(String, String)>,
    ) -> (Option<Callback>, Option<PathBuf>)
    {
        if let Some(last) = segments.last() {
            if last.contains('.') {

            }
        }

        (RouteTrie::recursive_find(&route_head.root, segments, params), None)
    }

    fn recursive_find(
        root: &Node,
        segments: &[String],
        params: &mut Vec<(String, String)>,
    ) -> Option<Callback>
    {
        //TODO: return tuple options instead of just callback option

        if segments.is_empty() {
            return None;
        }

        let head = &segments[0];
        let is_segments_tail = segments.len() <= 1;

        if let Some(child) = root.named_children.get(head) {
            if is_segments_tail {
                return child.callback;
            }

            return RouteTrie::recursive_find(&child, &segments[1..], params);
        }

        for param_node in root.params_children.iter() {
            if param_node.field.name.is_empty() {
                continue;
            }

            if let Some(ref reg) = param_node.field.validation {
                if !reg.is_match(head) {
                    continue;
                }
            }

            params.push((param_node.field.name.clone(), head.clone()));

            if is_segments_tail {
                return param_node.callback;
            }

            if let Some(cb) = RouteTrie::recursive_find(param_node, &segments[1..], params) {
                return Some(cb);
            }

            params.pop();
        }

        None
    }
}

impl Clone for RouteTrie {
    fn clone(&self) -> Self {
        RouteTrie {
            root: self.root.clone(),
        }
    }
}
