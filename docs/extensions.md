# Writing osm-gpui extensions

Extensions add features to osm-gpui without changing the app: menu commands,
pins on the map, sections in the side panel, and calls to web services. They
are WebAssembly components that run sandboxed inside the app, in the same
spirit as Zed's extensions. [MapRoulette](../extensions/maproulette) is a
complete example.

This guide covers writing one in Rust, trying it locally, what the API
offers, and publishing it.

- [How an extension works](#how-an-extension-works)
- [Quick start](#quick-start)
- [The manifest](#the-manifest-extensiontoml)
- [The API](#the-api)
- [Panels](#panels)
- [Markers](#markers)
- [Following the map and the selection](#following-the-map-and-the-selection)
- [Networking](#networking)
- [Storage and secrets](#storage-and-secrets)
- [Translations](#translations)
- [Uploads](#uploads)
- [Limits](#limits)
- [Testing](#testing)
- [Publishing](#publishing)
- [API versions](#api-versions)
- [Troubleshooting](#troubleshooting)

## How an extension works

An extension is a folder with:

| File | What it is |
|---|---|
| `extension.toml` | The manifest: id, name, version, the menu commands, and the capabilities it asks for. |
| `extension.wasm` | The code: a WebAssembly component built for `wasm32-wasip2`. In a Rust crate this is built for you. |
| `locales/<lang>.toml` | Optional translations. |

The app calls the extension when something happens: one of its menu commands
is chosen, one of its pins is clicked, a button in one of its panels is
pressed. The extension does its work, which may include HTTP requests, and
returns. Anything it wants the app to change (show pins, open a panel, move
the map, download data) it asks for during the call. The app applies those
requests, in order, once the call returns.

So an extension never draws or edits anything directly. It describes what
should be shown, and the app shows it.

Sandboxing rules:

- No files, sockets, environment variables or subprocesses.
- Network only through the app, over HTTPS, to the hosts the manifest lists.
- The keychain only if the manifest asks for it, and only for its own
  secrets.
- Each call has a cap on how much computation it may do, so a runaway loop
  ends the call instead of hanging the extension.
- Calls run off the UI thread, one at a time per extension. Anything that
  happens while the extension is busy (a click, a menu command) waits its
  turn. Of several waiting viewport or selection events, only the latest is
  delivered.

The interface is defined in WIT at
[`crates/osm-gpui-extension-api/wit/extension.wit`](../crates/osm-gpui-extension-api/wit/extension.wit).
The Rust SDK in that crate wraps it. Any language that can produce a
component for that world works, but this guide only covers Rust.

## Quick start

You need [rustup](https://rustup.rs) and the WebAssembly target:

```sh
rustup target add wasm32-wasip2
```

Create a library crate:

```sh
cargo new --lib hello-osm
```

`Cargo.toml`:

```toml
[package]
name = "hello-osm"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
osm-gpui-extension-api = { git = "https://github.com/iandees/osm-gpui" }
```

`extension.toml`, next to `Cargo.toml`:

```toml
id = "hello-osm"
name = "Hello OSM"
version = "0.1.0"
schema_version = 1
authors = ["You"]
description = "Says where the map is."

[[commands]]
id = "where"
label = "Where Am I?"
```

`src/lib.rs`:

```rust
use osm_gpui_extension_api::{self as osm, button, ButtonStyle, Element, Extension, Inputs, Panel};

struct Hello {
    clicks: u32,
}

impl Hello {
    fn panel(&self) -> Panel {
        let v = osm::viewport();
        Panel {
            title: "Hello".into(),
            elements: vec![
                Element::Heading("The map is showing".into()),
                Element::Property((
                    "Centre".into(),
                    format!(
                        "{:.5}, {:.5}",
                        (v.min_lat + v.max_lat) / 2.0,
                        (v.min_lon + v.max_lon) / 2.0
                    ),
                )),
                Element::Property(("Zoom".into(), format!("{:.1}", osm::zoom()))),
                Element::Muted(format!("Refreshed {} times", self.clicks)),
                Element::Buttons(vec![button("refresh", "Refresh", ButtonStyle::Primary)]),
            ],
        }
    }
}

impl Extension for Hello {
    fn new() -> Self {
        Hello { clicks: 0 }
    }

    fn run_command(&mut self, id: &str) -> osm::Result<()> {
        match id {
            "where" => {
                osm::show_panel("main", Some(&self.panel()));
                Ok(())
            }
            other => Err(format!("unknown command {other}")),
        }
    }

    fn panel_button(&mut self, _panel: &str, button: &str, _inputs: &Inputs) -> osm::Result<()> {
        if button == "refresh" {
            self.clicks += 1;
            osm::show_panel("main", Some(&self.panel()));
        }
        Ok(())
    }
}

osm::register_extension!(Hello);
```

To install it, open **Settings > Extensions**, press **Install Dev
Extension…** and pick the crate's folder. The app runs `cargo build --release
--target wasm32-wasip2` there, installs the result, and loads it. **Tools >
Where Am I?** then opens the panel.

After changing the code, press **Rebuild** on the extension in **Settings >
Extensions**. Dev extensions remember their source folder,
and they are never replaced by updates from the index.

If a folder contains a prebuilt `extension.wasm` next to its
`extension.toml`, Install Dev Extension uses that file instead of building.

## The manifest (`extension.toml`)

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Lowercase letters, digits and dashes, up to 64 characters. Must not change once published: it keys storage, secrets and updates. |
| `name` | yes | Shown in menus and the manager. A translation can replace it (see [Translations](#translations)). |
| `version` | yes | The extension's version, such as `1.2.0`. Updates are offered when the index has a higher one. |
| `schema_version` | yes | Always `1`. |
| `authors` | no | List of names. |
| `description` | no | One sentence, shown in the manager. |
| `repository` | no | Where the source lives. |
| `[capabilities]` | no | What the extension may reach (see below). |
| `subscribe` | no | Events to hear about: `"viewport"`, `"selection"` (see [Following the map and the selection](#following-the-map-and-the-selection)). Put it before any table. |
| `[[commands]]` | no | Menu entries: `id`, `label`, and optionally `menu` and `toggle` (see below). |

Capabilities:

```toml
[capabilities]
# Hosts http_fetch may reach, over HTTPS only. Bare host names: no scheme,
# port, path or wildcard. "example.org" also allows "api.example.org".
http = ["maproulette.org"]
# May use secret_get / secret_set (the system keychain).
secrets = true
```

The manager shows these to the user before installing, and the app enforces
them:

- A request to a host that isn't listed fails without being sent.
- Without `secrets = true`, secret reads find nothing and writes are
  dropped.

There is no menu of its own for each extension. A command goes in the app's
menus next to the app's own items of the same kind, chosen with `menu`:

| `menu` | Where | For |
|---|---|---|
| `overlays` | View, after Show Notes and To-do List | Things drawn over the map: photos, tasks. |
| `imagery` | Imagery, after the ways to add imagery | Adding imagery layers. |
| `data` | File, after Download from OSM and Download GPS Traces | Loading data layers. |
| `tools` (the default) | Tools, after the selection actions | Anything else. |
| `help` | Help | Help and links. |

Commands appear in the order they are listed, after a separator. Choosing one
calls `run_command` with its `id`. Name a command so it reads well among the
app's own items, without your extension's name in front unless the label
needs it ("Mapillary Photos", not "Mapillary: Show").

A command with `toggle = true` is drawn with a checkmark, for something
that is on or off. It starts unchecked. Call `set_command_checked(id, on)`
whenever it changes, including in `new()` when you restore saved state.

```toml
[[commands]]
id = "photos"
label = "Mapillary Photos"
menu = "overlays"
toggle = true
```

Settings that are not actions, such as an API key, don't belong in the
menus: return them from `settings()` (see [Settings](#settings)).

## The API

Everything is in the `osm_gpui_extension_api` crate. The examples here import
it as `osm`.

### What the app calls: the `Extension` trait

| Method | When |
|---|---|
| `new()` | Once, when the extension is loaded. Required. |
| `run_command(id)` | A `[[commands]]` entry was chosen. |
| `marker_clicked(id)` | One of your markers was clicked. |
| `panel_button(panel, button, inputs)` | A button in one of your panels was pressed. `inputs` holds the text of that panel's inputs. |
| `panel_closed(panel)` | The user closed one of your panels. |
| `settings()` | Your section of Settings > Extensions, or `None` (see [Settings](#settings)). |
| `viewport_changed()` | The map moved and settled (with `subscribe = ["viewport"]`). |
| `selection_changed()` | The selection changed (with `subscribe = ["selection"]`). |
| `changeset_tags()` | Asked after every call. Return the tags you want on the next upload (see [Uploads](#uploads)). |
| `upload_finished(changesets)` | An upload succeeded, in these changesets. |

Every method except `new` has a default that does nothing.

Returning `Err(message)` from a method shows the message in the status bar.
Anything you asked for before returning is still applied.

The extension object lives as long as the app does, unless the user turns it
off, updates it or uninstalls it. State kept in its fields survives between
calls, but not across restarts; use [storage](#storage-and-secrets) for that.

### What you can call

| Function | Does |
|---|---|
| `viewport() -> Bounds` | The visible area when this call started. |
| `zoom() -> f64` | The zoom when this call started. |
| `selection() -> Vec<FeatureRef>` | The selected OSM features (kind and id) when this call started. |
| `fly_to(at, zoom)` | Centre the map on `at` at `zoom`. |
| `download_area(bounds)` | Download OSM data for `bounds` into the editing layer, as File > Download from OSM does for the visible area. |
| `set_status(text)` | Show a message in the status bar. |
| `open_url(url)` | Open an `http` or `https` URL in the browser. |
| `set_markers(&[Marker])` | Replace all of your markers. |
| `show_panel(id, Option<&Panel>)` | Show or replace panel `id`; `None` closes it. |
| `set_command_checked(id, on)` | Check or uncheck a `toggle = true` command. |
| `http_fetch(&HttpRequest)` | Send an HTTP request (see [Networking](#networking)). |
| `http_fetch_many(&[HttpRequest])` | Send several requests in parallel; each answer is at its request's position. |
| `get(url) -> HttpRequest` | A GET request for `url`, to send with either of the above. |
| `get_json::<T>(url)` | GET a URL and parse a 2xx JSON answer into `T`. |
| `storage_get(key)`, `storage_set(key, Option<&str>)` | Small persistent key-value storage. |
| `secret_get(key)`, `secret_set(key, Option<&str>)` | Keychain secrets (needs `secrets = true`). |
| `locale() -> String` | The app's UI language, such as `de` or `fr-CA`. |
| `t(key)`, `t_args(key, &[(name, value)])` | A message from your own translations. |
| `log(text)` | Write a line to the app's stderr. |

`viewport`, `zoom`, `selection` and `locale` describe the app when the call
started. They do not change during a call, even after you call `fly_to`.

## Panels

A panel is a collapsible section in the side panel, under the selected
feature's sections, with your extension's name in its header. An extension
may have any number of panels open at once, each identified by an id you
choose. Calling `show_panel` with an id that is already open replaces that
panel in place: it keeps its position, whether it is collapsed, and any text
typed into inputs whose ids haven't changed.

A `Panel` is a title and a list of elements, laid out top to bottom:

| `Element` | Shows |
|---|---|
| `Heading(text)` | Bold text. |
| `Text(text)` | Body text. Wraps. |
| `Muted(text)` | Smaller, de-emphasised text. |
| `Property((label, value))` | A label and its value on one line. |
| `Buttons(Vec<Button>)` | A row of buttons. Pressing one calls `panel_button` with the panel's id and the button's `id`. |
| `Input(TextInput)` | A text box with an optional label. `value` is only its starting text. Set `secret: true` to mask it. |
| `Link((label, url))` | A link that opens `url` in the browser. |
| `Picture(Image { url, alt })` | A picture the app downloads (from a host your manifest allows) and shows at the panel's width, letterboxed; a double-click opens it full size over the window, with zoom and pan. `alt` is shown if it can't be loaded. |
| `Divider` | A thin rule. |

`button(id, label, style)` builds a `Button`. Its styles are `Primary`,
`Secondary`, `Danger` and `Ghost`. Set `enabled: false` to grey a button out.
A button pressed while the extension is busy is handled when it is free; the
panel's header says "Working…" meanwhile.

You only find out what was typed in an input when a button in the same panel
is pressed: `inputs.get("input-id")` returns its current text.

When the user closes a panel with its × button, the app calls
`panel_closed(id)`. Update your state there if the panel stood for something,
for example by deselecting the marker it described.

## Settings

Each installed extension has a row in **Settings > Extensions** that opens to
show what it may reach and, if it has any, its own settings. Return them from
`settings()` as a [`Panel`](#panels):

```rust
fn settings(&mut self) -> Option<Panel> {
    Some(Panel {
        title: osm::t("key.title"),
        elements: vec![
            Element::Input(TextInput {
                id: "key".into(),
                label: osm::t("key.label"),
                placeholder: String::new(),
                value: String::new(),
                secret: true,
            }),
            Element::Buttons(vec![button("save", &osm::t("key.save"), ButtonStyle::Primary)]),
        ],
    })
}
```

A button there calls `panel_button` with the panel id `osm::SETTINGS_PANEL`
(`"settings"`) and the text of the section's inputs. While Settings is open,
the app asks `settings()` again after every call to your extension, so the
section always shows the current state: you never show or close it yourself.
Pictures are not shown there.

## Markers

```rust
osm::set_markers(&[Marker {
    id: "42".into(),                           // passed to marker_clicked
    at: LatLon { lat: 40.7141, lon: -74.006 },
    color: 0x7c3aed,                           // 0xRRGGBB
    selected: false,                           // drawn larger, with a blue halo
    heading: None,                             // or Some(degrees) for a camera
}]);
```

Markers go in a layer of their own, named after the extension, in the Layers
list. The user can hide it there. Each call to `set_markers` replaces all of
the extension's markers; an empty list removes them.

A marker with a `heading` (degrees clockwise from north) is drawn as a
smaller dot with a wedge pointing that way, as for a photo and the direction
the camera faced. With more than 1,500 markers, all but the selected ones are
drawn as small plain dots to keep the map fast. A click goes to the nearest
marker within reach.

## Following the map and the selection

An extension that shows things for the visible area (photos, tasks) can be
told when the view changes instead of waiting for a command. List the events
in the manifest, before any table:

```toml
subscribe = ["viewport", "selection"]
```

- `viewport_changed` is called once the map has stopped moving for a
  moment, not during a drag or a zoom. Read the new view with `viewport()`
  and `zoom()`. A typical extension fetches what it is missing for the view
  with `http_fetch_many`, caches it, and calls `set_markers`.
- `selection_changed` is called when the selected features change. Read
  them with `selection()`. Only send a selected feature's id to a service
  when the user has turned that on: it tells the service what they are
  looking at.

Extensions that don't subscribe are never called for these. The
[street-photos](../extensions/street-photos) crate (shared by the Mapillary,
Fotoshi and Panoramax extensions) is a complete example of following the
view by map tile.

## Networking

```rust
let resp = osm::http_fetch(&HttpRequest {
    method: HttpMethod::Put,
    url: format!("https://maproulette.org/api/v2/task/{id}/1"),
    headers: vec![("apiKey".into(), key)],
    body: None,
})?;
if (200..300).contains(&resp.status) { /* ... */ }
```

- Only `https://` URLs to hosts listed in `[capabilities] http` are sent.
- `Err` means the request was refused, or the connection itself failed (DNS,
  TLS, timeout). Any answer from the server, including 4xx and 5xx, comes
  back as `Ok`, so check `status`.
- Methods are `Get`, `Post` and `Put`. `Delete` is not supported yet.
- A request body must be UTF-8. Set `Content-Type` yourself in `headers`.
- `http_fetch_many` sends a batch a few at a time in parallel. Use it to
  fetch several map tiles at once; one `http_fetch` after another would wait
  for each in turn.
- Requests go through the app's own HTTP client, with its timeouts and its
  User-Agent.

## Storage and secrets

**Storage** (`storage_get` and `storage_set`) is a small JSON file private to
the extension. Use it for preferences and for state that should survive a
restart. It is not encrypted, so don't put keys or passwords in it.

**Secrets** (`secret_get` and `secret_set`) go in the system keychain, or in
an owner-only file where there is no keychain. They are namespaced by
extension id, so one extension cannot read another's. They need
`secrets = true` in the manifest. Setting `None` or an empty string deletes
a secret.

The first read of a secret in a session can bring up the system's keychain
prompt; later reads come from memory. Read secrets when you need them, not
on every redraw.

Storage and secrets are kept when an extension is uninstalled, so
reinstalling it restores them.

## Translations

Put one TOML file per language in `locales/`, named by language tag:
`en.toml`, `de.toml`, `pt-BR.toml`. Nested tables become dotted keys.

```toml
# locales/de.toml
name = "MapRoulette"
description = "MapRoulette-Aufgaben auf der Karte finden …"

[commands]
tasks = "MapRoulette-Aufgaben"

[panel]
load = "Daten laden"
tasks = "%{count} Aufgaben"
```

The app reads three kinds of key itself:

- `name` and `description`, for menus and the manager.
- `commands.<id>`, for menu labels.

If a key is missing, the app falls back to the manifest's value.

Every other key is yours. Look it up with
`osm::t("panel.load")`, or `osm::t_args("panel.tasks", &[("count", "3")])`
to fill in `%{count}`. A lookup tries the app's language (`de-AT`), then its
base language (`de`), then `en`, then returns the key itself, so a missing
message shows up as its key rather than as blank text.

Text that comes from a web service (a challenge's instructions, for example)
stays in whatever language the service sent.

## Uploads

`changeset_tags` is asked after every call to your extension. Return the tags
you want on the next upload:

- `comment` fills the upload dialog's comment, unless the user has already
  typed one.
- `hashtags` is appended to that comment when the comment doesn't already
  contain them.

Other tags are not used yet.

After a successful upload, `upload_finished` receives the changeset ids. Use
it to prompt the user to finish their task, or to report the changeset to
your service.

## Limits

| What | Limit |
|---|---|
| Computation per call | about 20 billion wasm instructions. Exceeding it ends the call with an error. Time spent waiting on HTTP does not count. |
| HTTP response body | 16 MB |
| Storage | 256 KB in total |
| A secret | 8 KB; keys up to 128 characters |
| A locale file | 512 KB |
| A published package | 64 MB |

## Testing

Keep the logic that does not call the host (parsing, geometry, building
panels from data) in plain functions, and test them natively with
`cargo test`. MapRoulette's tests do this.

A test that calls a host function panics outside the app, because there is
no app for the call to reach. To test the whole extension against the real
host, with scripted HTTP and in-memory secrets, see
[`src/extensions/tests.rs`](../src/extensions/tests.rs) in this repository.
It builds `extensions/maproulette` and drives it through the same calls the
app makes.

## Publishing

The extensions offered in **Settings > Extensions > Available** are
listed in [`registry/extensions.toml`](../registry/extensions.toml). To
publish yours, open a pull request that adds an entry:

```toml
[hello-osm]
repository = "https://github.com/you/hello-osm"
rev = "<full commit sha to build>"
path = "."          # the extension's folder in that repository
version = "0.1.0"   # must equal the version in its extension.toml
```

The pull request's CI builds the entry. Reviewers then check the code at
`rev` and the capabilities it asks for. Once it is merged, CI packages it and
publishes it to the index the app reads.

To publish an update, bump `version` in your `extension.toml`, then open a
pull request that changes `rev` and `version` in the registry. Users see an
**Update** button in the manager. Updates are never installed automatically.

To build the index locally, exactly as CI does:

```sh
cargo run --bin build-extension-index -- \
    --registry registry/extensions.toml --out dist --base-url https://example.test/
```

This writes `dist/index.json` and one `<id>-<version>.tar.gz` per extension.
A package contains `extension.toml`, `extension.wasm` and `locales/*.toml`,
and nothing else.

When installing from the index, the app checks the package's SHA-256, that
its id and version match the index entry, and that it was built against an
API version the app supports.

## API versions

The WIT package version (`osm-gpui:extension@0.3.0`) is the API version. The
SDK's version matches it, and the index records which version each extension
was built against.

**An extension keeps working when the API moves on.** The app runs
extensions built against every API version it lists in
`SUPPORTED_API_VERSIONS` (today 0.1.0, 0.2.0 and 0.3.0), not only the newest. An
extension built against an older version gets the host it was built for: it
simply doesn't see what came later. For example, a 0.1 extension's markers
have no heading, and it is never sent viewport or selection events; an
extension older than 0.3 has no section in Settings and can't set a
checkmark. You only
need to rebuild against a newer SDK to use what it adds.

An extension built against a *newer* version than the app knows doesn't
load, and the manager shows it as needing a newer osm-gpui instead of
offering to install it.

### Changing the API

This part is for people changing osm-gpui itself. A released API version
never changes. Any change to the WIT, even an addition, is a new version,
because a component links to the exact interface it was built against. To add
one:

1. Change `crates/osm-gpui-extension-api/wit/extension.wit` and bump its
   `package` version, the SDK crate's version, and `runtime::API_VERSION`.
   While the API is 0.x, bump the minor version.
2. Freeze the new WIT as `src/extensions/wit/v<major>_<minor>/extension.wit`
   and add its hash to `FROZEN` in `src/extensions/tests.rs`.
3. Keep the previous version running. Make its bindings a module beside
   `src/extensions/runtime/v0_1.rs`, generated from its frozen WIT. Its host
   functions convert their arguments to the new types and call the current
   implementation, and its exports join the `Guest` enum and the `dispatch!`
   macro. Exports it lacks are no-ops. If the previous version was the
   current one, the old current bindings become this module.
4. Add it to `SUPPORTED_API_VERSIONS`, and commit an extension built against
   it under `tests/fixtures/extensions/` with a test that loads and drives
   it, as `older_api_versions` does for MapRoulette 0.1.0 and 0.2.0.
5. Rebuild the extensions in `extensions/` against the new SDK when they need
   what it adds, and bump their versions so installed copies are offered the
   update.

`released_api_versions_never_change` fails if a frozen WIT is edited, or if
the SDK's WIT changes without a new version, so step 1 can't be skipped by
accident.

To drop support for an old version, delete its module, its frozen WIT, its
fixture and its test, and remove it from `SUPPORTED_API_VERSIONS` and
`FROZEN`. Extensions built against it then stop loading, so do it rarely and
say so in the changelog.

## Troubleshooting

**`can't find crate for core` / "the wasm32-wasip2 target may not be
installed"**: run `rustup target add wasm32-wasip2`. If you have a second
Rust installation (for example, Homebrew's) ahead of rustup's on `PATH`, the
build may use that one, which has no wasm target. Install Dev Extension
prefers rustup's toolchain when rustup is present. For a manual build, run
rustup's binaries explicitly:
`RUSTC=$(rustup which rustc) $(rustup which cargo) build --release --target wasm32-wasip2`.

**"was built against extension API …; this app runs …"**: the extension was
built against an API version this app doesn't run. That is usually a newer
SDK than the app: update osm-gpui, or build against the SDK version the app
lists.

**"may not reach …"**: the URL's host isn't in `[capabilities] http`, or the
URL isn't `https://`.

**Secrets always come back empty**: add `secrets = true` under
`[capabilities]`. If the extension is installed, reinstall or rebuild it so
the new manifest takes effect.

**A click seems to do nothing for a while**: the extension is still busy
with an earlier call, usually waiting on a slow HTTP request. The click is
handled when that call returns. Keep calls short, and fetch in parallel with
`http_fetch_many`.

**Where things are on disk**: installed extensions are in
`<config dir>/osm-gpui/extensions/installed/<id>/`, and storage is in
`…/extensions/data/<id>.json`. Set `OSM_GPUI_EXTENSIONS_DIR` to use a
different folder, and `OSM_GPUI_EXTENSION_INDEX` to read a different index,
which is handy for testing a local `dist/`.
