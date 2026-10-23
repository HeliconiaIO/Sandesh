# Credits

## Authors

Sandesh is developed and maintained by **[Heliconia Solutions Pvt. Ltd.](https://www.heliconia.io)**

Contributors before the first public release (the public repository starts from a fresh history,
so their work is recorded here):

- Hardik Nakum ([@hspl-hanak](https://github.com/hspl-hanak))
- Nevil Kiyada ([@hspl-navki](https://github.com/hspl-navki))
- Bhavesh Selarka ([@patel1008b](https://github.com/patel1008b))

Everyone who contributes after that appears in the repository's
[contributor graph](https://github.com/HeliconiaIO/Sandesh/graphs/contributors) and in each
release's notes.

## Built on

Sandesh depends on many open-source projects. A few that carry most of the weight:

| Project | Used for |
| --- | --- |
| [Tokio](https://tokio.rs) | Async runtime under every listener |
| [Axum](https://github.com/tokio-rs/axum) and [Hyper](https://hyper.rs) | REST API and WebSocket |
| [Rustls](https://github.com/rustls/rustls) | TLS for SMTP, IMAP, POP3 and HTTP |
| [SQLx](https://github.com/launchbadge/sqlx) and [SQLite](https://sqlite.org) | Mail storage and full-text search |
| [mail-parser](https://github.com/stalwartlabs/mail-parser) | MIME parsing |
| [utoipa](https://github.com/juhaku/utoipa) | OpenAPI contract generation |
| [Argon2](https://github.com/RustCrypto/password-hashes) | Password hashing |
| [Tauri](https://tauri.app) | Desktop application |
| [React](https://react.dev), [React Router](https://reactrouter.com), [TanStack Query](https://tanstack.com/query) | Web console |
| [Lucide](https://lucide.dev) | Icons |
| [Inter](https://rsms.me/inter/), [JetBrains Mono](https://www.jetbrains.com/lp/mono/), [Space Grotesk](https://github.com/floriankarsten/space-grotesk) | Typefaces (SIL OFL-1.1, via Fontsource) |

The full list, with every licence text, ships as `THIRD_PARTY_LICENSES.txt` in each release.

## Prior art

[Mailpit](https://github.com/axllent/mailpit) and [GreenMail](https://github.com/greenmail-mail-test/greenmail)
were studied as prior art for developer experience and protocol scope. Sandesh is an independent
implementation, and no code, UI or branding is taken from either.

## Security researchers

People who report vulnerabilities responsibly are credited in the matching GitHub Security
Advisory, unless they ask not to be.
