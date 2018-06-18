# 2018-06
## 0.3.2
- Update 'session' module to be more robust for use with generic session data types.
//TODO: give examples...

## What's new in 0.3.1
- Fixing a few obvious bugs and improve the performance.
- Now the template framework is mostly done. A simple template engine will be added in the next main version (0.3.3).

# 2018-05
## Major version break: 0.3.0
0.2.x versions are good experiments with this project. But we're growing fast with better
features and more performance enhancement! That's why we need to start the 0.3.x versions
with slight changes to the interface APIs.

## Migrating from 0.2.x to 0.3.0
Here're what to expect when updating from 0.2.x to 0.3.0:

- The route handler function's signature has changed, now the request and response objects
are boxed! So now your route handler should have something similar to this:
```rust
pub fn handler(req: &Box<Request>, resp: &mut Box<Response>) {
    /// work hard to generate the response here...
}
```

- The `StateProvider` trait is deprecated (and de-factor no-op in 0.3.0), and it will be removed in
the 0.3.3 release. Please switch to use the `ServerContext` features instead. You can find how to
use the `ServerContext` in this example: [Server with defined router](https://github.com/Chopinsky/Rusty_Express/blob/master/examples/use_router.rs)
