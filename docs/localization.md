# Contributing translations

UniDirStat bundles UTF-8 JSON catalogs in `locales/en.json` and `locales/ru.json`. English text is the stable lookup key; each catalog must contain the same keys. Translations are compiled into the executable and never downloaded. The GUI follows the native system language by default (`ru` selects Russian; other locales select English), or uses the persisted English/Russian choice in Settings.

Use `crate::i18n::tr("A static caption")` for GUI text. Add the caption to both catalogs. For a sentence containing values, use named placeholders:

```rust
trf("Saved {path}", &[("path", path.display().to_string())])
```

Keep placeholder names unchanged in every translation; reorder them freely. Format numeric values before passing them to `trf`. Values are inserted once, so braces inside filenames are never interpreted. Do not concatenate translated fragments to form sentences. Filenames, paths, hashes, regular expressions, owner IDs, saved filter names, and machine-readable report keys retain their original values. Saved filter names are user content, not catalog keys. Technical operating-system error details and native ACL command output can remain in their source language; translate the GUI context surrounding them.

`Language::{System, English, Russian}` is serialized using `system`, `english`, and `russian`. `set_language(Language)` changes the active GUI catalog; `tr(&str) -> &'static str` and `trf(&str, &[(&str, String)]) -> String` read it without filesystem access. Missing keys and malformed catalogs produce visible diagnostic markers instead of silently falling back from Russian to English. An unresolved placeholder remains visible, which makes incorrect calls noticeable.

To add a language, copy the English catalog, translate its values, add the enum/catalog/system-locale mapping in `src/i18n.rs`, and add the language to the Settings selector. Keep old enum values stable. Use short navigation and button labels, and check the native window at its minimum supported size and with a larger interface scale. Use painted shapes for decorative icons rather than relying on uncommon font glyphs.

Run `cargo test --all-features --locked`, `cargo fmt --all -- --check`, and `cargo clippy --all-targets --all-features --locked -- -D warnings`. Catalog tests verify matching keys and placeholders, every literal `tr`/`trf` call in `src/app.rs`, Russian locale detection, and literal insertion of filenames containing braces. Add translation calls with literal keys so this coverage check can find them.

Old settings remain compatible through serde defaults. `--settings FILE` uses an explicit portable JSON settings file, limited to 1 MiB and saved through atomic replacement. A missing file starts with defaults. Invalid or oversized input is reported and preserved; it is detached from automatic saving so that closing the application cannot overwrite a corrupt file.
