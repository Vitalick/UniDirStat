[English](guide.md) | [Русский](guide.ru.md)

[← Installation and project overview](../README.md)

# UniDirStat — Documentation

- [Build and run](#build-and-run)
- [Features](#features)
- [Using the GUI](#using-the-gui)
- [Headless CLI](#headless-cli)
- [Limits](#limits)
- [Checks](#checks)
- [Releases](#releases)
- [License](#license)

## Build and run

Requires **Rust 1.96 or newer**. CI uses Rust 1.96.0.

```sh
cargo run --release --locked
cargo run --release --locked -- ~/Downloads ~/Documents
cargo run --release --locked -- --settings ./unidirstat-settings.json ~/Downloads
```

The default build includes the native GUI. Multiple folders, mounted volumes, and network paths exposed by the OS can be scanned together. On macOS, a first-run access guide explains optional Full Disk Access before scanning. Only you can enable this in System Settings; limited scanning remains available. Protected paths and local ad-hoc build updates can still require OS permission prompts.

- **macOS:** install Xcode Command Line Tools. `sh scripts/package-macos.sh` creates `target/packages/UniDirStat-<version>-macos-<architecture>.dmg` containing the app and an Applications shortcut. The script prints the separately staged app path. Local ad-hoc signing is used; notarization has not been performed.
- **Windows:** use Rust MSVC, Visual Studio Build Tools and Inno Setup 6. Run `./scripts/package-windows.ps1` from PowerShell to create the x86_64 setup installer in `target/packages/`.
- **Linux:** on Debian/Ubuntu, install `pkg-config`, `libxkbcommon-dev`, `libwayland-dev`, and `libgl1-mesa-dev`. Native dialogs require `xdg-desktop-portal` and its desktop backend. `sh scripts/package-linux.sh` builds x86_64 AppImage and `.deb` packages; it also needs `desktop-file-utils`, `patchelf`, `dpkg-deb`, and the `LINUXDEPLOY` executable.

Windows installers currently have no trusted code signature, so Windows may show an unknown publisher or a SmartScreen warning. An icon or version resource does not establish publisher identity. Download only from the project release page.

## Features

- **Parallel scans:** a bounded worker pool, multiple roots with overlapping paths counted once, progress, pause/resume, cancellation, partial results, full refresh, and selected-folder recalculation. After moving an item to the trash, its parent folder is recalculated automatically. Set workers to `0` for automatic selection or `1..64` explicitly.
- **File browser and search:** a virtualized directory tree and largest-file list; sorting by name, size, or modified time; asynchronous indexed parallel text/regex searches by name or path. Filters cover minimum/maximum size and modified age, kind, extension, owner UID where available, and folders empty in the scan. Save and reuse named filter profiles.
- **Linked workspace:** an expandable size-descending file tree above the map, resizable panes and columns, parent-relative size bars, shared multi-selection and folder-region highlights. Clicking a map item reveals it in the tree. The side panel shows file types and largest files for the focused folder.
- **Storage maps:** row-packed hierarchical treemap, readable flame graph, and sunburst with clipped/ellipsized labels, same-color tiny-file groups within each folder, pointer-anchored wheel zoom, drag panning and animated double-click focus. Groups show their actual file count and size; zoom reveals individual files. Extension colors follow global logical-size ranking, stay fixed while exploring folders, and reuse the muted palette after the first 17 categories. Rectangles meet without gaps; dark borders and a soft bevel distinguish adjacent blocks. Zoom out and Reset remain explicit controls.
- **Appearance and language:** persistent System/Light/Dark themes and System/English/Russian choices, visible above Settings in the sidebar or compact top navigation. System checks preferred locales in order for supported English/Russian variants, otherwise English. Missing translations fall back to English. Regular-weight text, compact search and summary, default 80% interface scale adjustable from 75% to 200%, reduced motion and configurable Modified/Share columns. Saved scale choices are retained. Short windows switch between tree and map; Summary and Search remain available.
- **Responsive interaction:** map generation runs in a cancellable background job with a small cache; returning from Overview to the same file view preserves its rows. File rows, overview previews and maps have context menus. Cmd/Ctrl toggles selection, Shift selects row ranges, and Cmd/Ctrl+A selects the current file list; selected paths can be copied together. Cleanup remains per-item with confirmation.
- **Visible progress:** the directory tree fills during whole scans, with a spinner for active subtrees, an orange pause icon for folders waiting for a worker, and a green check only after the entire subtree finishes. After completion, a yellow warning triangle means some entries were read successfully and others failed; a red exclamation mark means none could be read. Cancelled folders remain marked separately. The same tree component, columns, expansion state and scroll position are used during and after scanning. The Scan time column records metadata work for files and subtree wall time for folders; parallel folder times are not additive. Older snapshots show unavailable timing. Scanning shows the last directory, logical/allocated bytes and elapsed time; finalization, indexing, sorting, filtering and result assembly show actual stage counters. Map counters show discovered work rather than a guessed overall percentage. A stalled scan suggests checking filesystem operations or a permission dialog.
- **Logical and allocated size:** Unix blocks and Windows allocation metadata where available. Logical bytes count each path; allocated bytes count identified hard links once. Unavailable metadata is marked.
- **Exact duplicates:** bounded parallel SHA-256 or SHA-512 after grouping by size. Larger same-sized candidates first compare small sampled regions to reject obvious differences; a match still requires a full-file hash. Empty files and identified hard links are not counted as reclaimable duplicate copies. Changed, replaced, or inaccessible files are rejected with errors.
- **Snapshots and reports:** validated JSON and UniDirStat scan CSV import/export, plus JSON/CSV duplicate and permission-metadata reports. CSV import accepts older scan formats; current files preserve per-entry scan time, depth cutoffs and completed warning/error states. Reports use temporary files and atomic replacement.
- **Permissions and insights:** read-only status, Unix mode/owner UID, file-age buckets, and configurable estimated monthly storage cost. Inspect native ACL output using `/bin/ls -lde` on macOS, optional `getfacl` on Linux, or `icacls` on Windows; ACL editing is not implemented.
- **Filesystem monitoring:** native change notifications trigger recalculation after a two-second debounce. Known changes inside the most recently recalculated folder refresh that subtree; other or ambiguous events trigger a full rescan. Watcher failures are shown; OS limits apply to large or network filesystems.
- **File actions:** open, reveal, copy path, confirmed system-trash cleanup, and atomic file/folder moves within one filesystem without overwriting existing destinations. Empty-folder cleanup checks again that the live folder is empty before trashing it.
- **Portable preferences:** use `--settings FILE` for GUI preferences in an explicit JSON file instead of the OS application-settings location. Missing files start with defaults; invalid files are reported and preserved. The import limit is 1 MiB.

Scan depth defaults to 256 (the selected root is level 0). Change **Maximum scan depth** in Settings, or use `--max-depth N`; `0` disables the limit. Folders at the limit retain their metadata, but their contents are not scanned. Totals are marked incomplete, and these folders are excluded from empty-folder results. Map depth defaults to 6 displayed levels and can be changed separately in Settings; opening a folder reveals the next levels without rescanning.

Scanning reads metadata, not file contents. Duplicate detection reads contents only when requested. Symbolic links and Windows reparse points are never traversed.

Directory identities are visited once, including macOS firmlink aliases. The macOS boot System/Data volumes appear as one card when their shared directory namespace is verified; other volumes remain separate.

Scan progress distinguishes logical bytes from allocated blocks. Logical size can exceed disk capacity because of sparse files and hard links. Allocation is not exclusive physical usage: APFS clones can share blocks, while snapshots and filesystem metadata are not included. Unknown allocation is shown as unavailable, not replaced by logical size. After updating from an earlier version, run a new scan to replace old totals.

## Using the GUI

1. Choose **Choose drives** in navigation to return to the drive/folder selection screen at any time. Existing results stay available through Overview until you start another scan. Selecting a drive or Add folder on this screen replaces the scan locations; Add folder in the results view extends them.
2. Select a file in a table or map to open its details and available actions.
3. Use the wheel over a map to zoom at the pointer, drag to pan, or double-click a folder for animated focus. **Zoom to selection** focuses the selected folder; **Zoom out / Reset** returns. Side statistics follow the focused folder; visual panning alone does not change that scope. Tree filters do not hide unrelated map regions.
4. Use **All files → Advanced filters** for search and saved profiles. **Duplicates** selects the hash algorithm and exports results; **Reports** opens/saves snapshots and exports scan or permission metadata.
5. Use the selected folder's **Refresh this folder**, **Inspect system ACL**, or confirmed cleanup actions when available. Empty-folder candidates require a completed scan. Exclusions and filesystem boundaries can hide contents; cleanup always rechecks live emptiness.
6. In **Settings**, choose language, theme, interface scale, columns, map styling, worker count, exclusions, allocation mode, and monitoring.

| Shortcut | Action |
| --- | --- |
| `F5` | Refresh all scan roots |
| `F6` | Pause/resume the active scan or duplicate job, including preparation |
| `Esc` | Cancel the active cancellable scan or duplicate job |

Pause/cancel requests are checked during traversal and result preparation. A current operating-system filesystem call may need to return before a request takes effect.

Imported snapshots are read-only and never authorize file changes. **Refresh** performs a new live scan on this computer before cleanup becomes available. Scan roots cannot be moved or trashed; scan their parent to manage them.

## Headless CLI

The CLI remains usable without GUI dependencies:

```sh
cargo build --release --no-default-features --locked
./target/release/unidirstat --help
./target/release/unidirstat --scan /data --threads 8 --export scan.json
./target/release/unidirstat --load scan.json --export scan.csv
./target/release/unidirstat --load scan.csv --search '\.rs$' --regex --name-only --max-size 1048576 --kind file --sort name --ascending
./target/release/unidirstat --scan /data --duplicates duplicates.csv --hash sha512 --threads 8
./target/release/unidirstat --load scan.json --permissions permissions.csv
./target/release/unidirstat --scan /data --empty-directories
```

These shell examples use macOS/Linux paths; on Windows, use the corresponding paths and `.\target\release\unidirstat.exe`.

- Input/output: repeat `--scan FOLDER`, use positional folders, or `--load FILE` for JSON/CSV. `--export FILE`, `--duplicates FILE`, and `--permissions FILE` select JSON/CSV by extension and imply headless mode.
- Performance/exclusions: `--max-depth 0..65535`, `--threads 0..64`, repeatable `--exclude REGEX`, and `--one-file-system` on Unix.
- Text search: `--search TEXT`, `--regex`, and `--name-only`. Search/filter flags imply headless mode.
- Ranges: `--min-size` / `--max-size` use bytes; `--min-age` / `--max-age` use modified-age days. GUI size filters use MiB.
- Other filters: repeatable `--kind file|directory|symlink|other`, `--extension EXT`, `--owner UID`, and `--empty-directories`.
- Search order/size: `--sort size|name|modified`, `--ascending`, and `--allocated`.
- Duplicate algorithm: `--hash sha256|sha512`, default `sha256`.
- Desktop preferences: `--settings FILE` selects portable GUI settings; headless operations use their explicit CLI arguments.
- General: `--no-gui`, `--help`, and `--` to treat remaining arguments as paths.

CLI summaries report scan errors; reports preserve available results and error information. A partial scan is not a complete inventory.

## Limits

Not implemented: raw NTFS/MFT acceleration, MTP access, deleted-file recovery, NTFS compression/deduplication and OS cleanup integrations, ACL editing, custom cleanup commands/plugins, permanent deletion, emptying the system trash, shell/Explorer integration, and Hilbert/Moore layouts. There is no mobile/browser build with arbitrary disk access.

A scan observes a changing filesystem; it is not an atomic filesystem snapshot. File actions require confirmation and current metadata validation. Cross-filesystem moves fail without deleting the source. Windows identity uses a 64-bit file index; 128-bit ReFS identity is not supported. On Unix, paths containing invalid UTF-8 cannot be exported to JSON/CSV.

JSON/CSV snapshot imports are limited to 256 MiB. All scan nodes and the search index remain in memory. Files in the same directory share their stored path prefix to reduce memory use; exported snapshots retain full paths. Map contents too small to distinguish are grouped into proportional regions that reveal more detail when zoomed; display depth defaults to 6 levels, flame also adapts to readable bands, and the GUI sunburst shows up to 12. Zoom into a deeper folder to continue. Storage cost is an estimate, not a provider bill. Technical OS errors and native ACL output may retain their source language.

## Checks

```sh
cargo fmt --all -- --check
cargo test --all-features --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo build --release --locked
cargo test --no-default-features --locked
```

## Releases

[GitHub Actions](../.github/workflows/ci.yml) runs formatting, all-feature tests, Clippy, headless tests and native packaging only when a tag matching `v*` is pushed. Branch pushes and pull requests do not start this workflow, and there is no manual Run workflow trigger. Each run saves versioned installers as workflow artifacts before publishing a release.

The version comes from `[package].version` in `Cargo.toml` and is included in installer filenames and application package metadata. A release tag must match it exactly: `v0.1.2` for version `0.1.2`. A mismatch fails the workflow before packaging.

To publish a release after the repository is on GitHub:

1. Set the new package version in `Cargo.toml`, run `cargo check` to update `Cargo.lock`, run the checks above, and commit both version files on `main`.
2. Tag that commit and push the branch and tag; for the current `0.1.2` version:

   ```sh
   git tag -a v0.1.2 -m "UniDirStat 0.1.2"
   git push origin main
   git push origin v0.1.2
   ```

3. Actions builds all five assets, verifies their complete set, uploads them to a draft release and downloads them again to verify SHA-256 hashes. Only then is the release published. Stable tags become Latest; tags such as `v0.2.0-beta.1` become prereleases and do not replace Latest. A failed run does not publish a new release; it may leave a draft for inspection or retry.

Publication uses the built-in `GITHUB_TOKEN`; only the release job has `contents: write`. No personal token is required. If a tagged run fails, fix the problem before creating the next version tag, or use **Re-run failed jobs** when no code changes are needed. A successful retry can publish the release for that same tag. Published installers are available from the [latest release](https://github.com/Vitalick/UniDirStat/releases/latest).

Successful packaging does not replace runtime validation on each supported platform. Translation contributions: [localization guide](localization.md).

## License

**GPL-3.0-or-later**. Treemap layout attribution is in [NOTICE](../NOTICE). The bundled Ubuntu font retains its [Ubuntu Font Licence](../assets/fonts/Ubuntu-LICENSE.txt).
