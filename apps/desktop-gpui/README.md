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
- `crates/t3-desktop`: the GPUI app. It owns the server process and the connection, and renders
  the UI from the same state every other client reads.

## Development

Run `vp i` at the repository root first: the app starts the server from `apps/server/src/bin.ts`.
On Linux, GPUI also needs the Wayland, xkbcommon, fontconfig, and Vulkan development packages.

```sh
cd apps/desktop-gpui
cargo run -p t3-desktop
cargo test
```

The app keeps its data in the checkout's `.t3`, like `vp run dev`, so it never touches `~/.t3`.
`vp run migrate-dev-db` seeds that directory with a copy of real data. Set `T3_DESKTOP_HOME` to use
another directory and `T3_NODE` to pick a Node runtime that is not first on `PATH`.

The `t3-host` integration test also starts the real server, with its data under `target/`.
