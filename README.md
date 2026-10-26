# YAM Lua runtime host

This is a host package, separate from the YAM v2 package manager. It owns the
Lua/Quasar API, embedded web interface, legacy `.yap` archives, `plugin.toml`,
and the runtime-specific permission model.

The generic `yam` core does not parse, install, authorize, or execute plugins.
Build the host with `cargo build --release --locked` in this directory. Package
it with the root YAM CLI after supplying the platform-target binary named by
`yam.yaml`.

`tools/convert_apkg.py` is an offline APKG-to-`.yap` converter for this runtime
only; it does not produce or install a v2 `.yampkg`.
