# T3 Code desktop (GPUI)

A native desktop client written in Rust with [GPUI](https://gpui.rs), replacing the Electron shell
in `apps/desktop`. It aims for feature parity with the existing desktop app, not a new product.

## Constraints

- The Node server in `apps/server` stays the source of truth. This app launches it and talks to it
  over the same protocol every other client uses; nothing here may require a server change.
- No Electron, Chromium, or embedded web views. Every surface is GPUI.
- Rust owns desktop concerns: windows, menus, shortcuts, notifications, and the server process
  lifecycle.

## Layout

- `crates/t3-client`: the client half of the protocol: Effect RPC over WebSocket with JSON frames,
  bearer-token and WebSocket-ticket auth, and the rotating desktop bootstrap token.
- `crates/t3-host`: launches the server with the `DesktopBackendBootstrap` envelope, waits for
  readiness, and stops it gracefully.

## Development

```sh
cd apps/desktop-gpui
cargo test
```

The `t3-host` integration test starts the real server from `apps/server/src/bin.ts`, so run
`vp i` at the repository root first. It needs a Node runtime that satisfies the server's
`engines.node`; set `T3_NODE` to use one that is not first on `PATH`. Its data directory lives
under `target/`, never under `~/.t3`.
