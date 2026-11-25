# osm-gpui-extension-api

Write [osm-gpui](https://github.com/iandees/osm-gpui) extensions in Rust.

An extension is a `cdylib` crate built for `wasm32-wasip2`, with an
`extension.toml` manifest next to its `Cargo.toml`. Implement `Extension` and
register it:

```rust
use osm_gpui_extension_api::{self as osm, Extension};

struct Hello;

impl Extension for Hello {
    fn new() -> Self {
        Hello
    }

    fn run_command(&mut self, id: &str) -> osm::Result<()> {
        osm::set_status(&format!("ran {id}"));
        Ok(())
    }
}

osm::register_extension!(Hello);
```

The full guide (manifest, panels, markers, networking, secrets, translations,
testing and publishing) is
[docs/extensions.md](https://github.com/iandees/osm-gpui/blob/main/docs/extensions.md).
The interface itself is [`wit/extension.wit`](wit/extension.wit); this
crate's version is the API version.
