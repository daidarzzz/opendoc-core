# Contributing to opendoc-core

Use stable Rust and run these checks from the repository root before submitting a change.

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo doc --no-deps
```

Use [the fixture guide](https://github.com/daidarzzz/opendoc/tree/master/src-tauri/tests/fixtures)
to obtain real docsets. Set `OPENDOC_FIXTURES` to that directory when
testing changes to scanning, indexing or search.

Keep HTTP, desktop integration and presentation in the consuming application.
For changes to a public API, document input expectations, errors and side
effects in Rustdoc. Check consumers when changing symbol ordering, path
resolution or the on-disk docset layout.

| Area | Source |
| --- | --- |
| Library loading and tarix cache reuse | [`src/library.rs`](src/library.rs) |
| Scanning, metadata and SQLite indexes | [`src/docset/`](src/docset/) |
| Symbol matching and ordering | [`src/search/`](src/search/) |
| Safe document path resolution | [`src/resolve.rs`](src/resolve.rs) |
| DevDocs package conversion | [`src/devdocs/`](src/devdocs/) |
