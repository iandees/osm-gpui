# Preset icon licences

The SVG files in this directory are the icons that the bundled iD tagging
schema presets refer to (`assets/presets/presets.json`). Each file is named
after the preset's `icon` value, `<set>-<name>.svg`. They are fetched, at the
pinned versions below, by `cargo run --example update_presets`; only icons a
preset (or the per-geometry fallback) references are kept.

Non-square viewBoxes are padded to a centred square by the fetch tool so they
render undistorted; the artwork itself is not modified.

| Prefix | Set | Version | Source | Licence |
| --- | --- | --- | --- | --- |
| `maki-` | Maki, Mapbox | 8.2.0 (npm `@mapbox/maki`) | https://github.com/mapbox/maki | CC0 1.0 Universal |
| `temaki-` | Temaki, Rapid Editor contributors | v5.13.0 | https://github.com/rapideditor/temaki | CC0 1.0 Universal |
| `iD-` | iD preset icons, iD contributors | v2.42.3 (`svg/iD-sprite/presets/`) | https://github.com/openstreetmap/iD | ISC |
| `roentgen-` | Röntgen, Sergey Vartanov | v0.16.0 | https://github.com/enzet/Roentgen | CC BY 4.0 |
| `fas-`, `far-` | Font Awesome Free, Fonticons, Inc. | 7.2.0 (via iD v2.42.3 `svg/fontawesome/`, which keeps the names the schema uses; Font Awesome 7.2.0 `svgs/` for names iD no longer carries) | https://github.com/FortAwesome/Font-Awesome | Icons: CC BY 4.0 |

No `fab-` (Font Awesome Brands) icon is referenced by the bundled schema, so
no brand icons are vendored.

## Attribution

These attributions are required by the CC BY 4.0 licence
(https://creativecommons.org/licenses/by/4.0/). The icons are used as-is
apart from the square viewBox padding noted above.

- Röntgen icons by Sergey Vartanov (CC BY 4.0),
  https://github.com/enzet/Roentgen. Files: `roentgen-*.svg`.
- Font Awesome Free icons by Fonticons, Inc. (https://fontawesome.com),
  licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/).
  Files: `fas-*.svg`, `far-*.svg`.

## Other licences

- Maki and Temaki icons are released under CC0 1.0 Universal
  (https://creativecommons.org/publicdomain/zero/1.0/); no attribution is
  required.
- The iD preset icons (`iD-*.svg`) are Copyright (c) iD Contributors, under
  the ISC licence:

  Permission to use, copy, modify, and/or distribute this software for any
  purpose with or without fee is hereby granted, provided that the above
  copyright notice and this permission notice appear in all copies.

  THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
  WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
  MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
  SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
  WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
  ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR
  IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
