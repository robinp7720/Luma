# Evolution mail troubleshooting

After upgrading Evolution or Evolution Data Server, rebuild and reinstall the
mail helper from the Luma checkout:

```sh
cargo build --release --locked --bin luma-mail-eds
install -Dm755 target/release/luma-mail-eds ~/.local/bin/luma-mail-eds
```

If the installed helper is already a symlink to that build output, only the
build command is needed. The build tracks installed Evolution/Camel libraries
and package metadata so Cargo relinks after native library upgrades. A helper
linked to removed libraries cannot start until it is rebuilt.

Evolution searches default to a five-second timeout to allow the source
registry to start on the first query. Existing configurations retain their
saved timeout; adjust it in the email integration settings when needed.

To check the configured helper through Luma's search provider in both All and
Email modes, choose a query matching a cached message's subject or sender:

```sh
LUMA_TEST_EMAIL_QUERY='known search term' cargo test --bin Luma \
  live_evolution_search_returns_openable_results -- --ignored
```

This test reads the current Luma configuration and cached mail. It checks that
openable results are produced without opening messages or sending mail.
