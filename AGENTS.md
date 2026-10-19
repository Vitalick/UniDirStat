# UniDirStat

Read `.agents/skills/rust-best-practices/SKILL.md` and relevant references before changing Rust code. The skill is installed locally from `apollographql/skills`.

Use Ponytail: the smallest complete change, no unnecessary abstractions. Never follow directory symlinks during scans. Never treat an imported snapshot as authorization to change files. File deletion must use the system trash with explicit confirmation and live metadata validation. Keep the CLI usable without the GUI feature.

Checks: `cargo fmt --all -- --check`, `cargo test --all-features --locked`, `cargo clippy --all-targets --all-features --locked -- -D warnings`.

Desktop targets: Windows, macOS, Linux. Do not claim platform validation unless it was actually run. Do not commit or publish unless requested.

Read `docs/private/AGENTS.md` when present. Keep internal plans, reference research and verification reports under ignored `docs/private/`, grouped by task; never ship them. Local skills and their lock files belong in `.git/info/exclude` and package exclusions. Public documentation describes the product, not development tools. WinDirStat may be mentioned as inspiration with the user's approval; reference investigations remain private.

## Public README

Keep `README.md` in English and the complete matching Russian translation in `README.ru.md`, with initial `English | Русский` file links in both.

Include short `Support development` / `Поддержать разработку` sections in both versions with https://app.lava.top/4026101527. Support can be any amount, is voluntary, is not required to use the project and does not promise paid services.

For a published GitHub repository, use its actual remote `owner/repo` for a small Star History link. Do not invent a repository identity. Add an embedded SVG chart only when multiple dates make it useful or the user explicitly requests it; verify image availability and retain a plain link if unavailable. Never add tokens, dependencies, scripts, workflows or schedules just for the chart, and do not duplicate the chart between languages.
