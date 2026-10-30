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

Development keeps its data in the checkout's `.t3`, like `vp run dev`, and ignores an ambient
`T3CODE_HOME`. `vp run migrate-dev-db` seeds that directory with a copy of real data.
`T3_DESKTOP_HOME` overrides the data directory in either mode; `T3_NODE` selects another Node runtime.
An explicit `T3_REPO_ROOT` runs that checkout's server sources, including from a release binary.

## Bundled runtime

Installed builds resolve resources beside the executable on Linux and Windows, or from
`Contents/Resources` in a macOS app bundle:

```text
resources/
  node/bin/node                  # node/node.exe on Windows
  server/apps/server/dist/bin.mjs
  server/node_modules/...
```

The server tree must include its production dependencies for the target platform and bundled Node
runtime. Installed builds use the existing desktop's T3 home (`~/.t3`, or `T3CODE_HOME`) and start the
server in the user's home, outside the resource directory. Release binaries require a bundle unless
`T3_REPO_ROOT` is explicit; missing resources fail rather than falling back to a build-machine path.

Assemble a portable directory (or a `.app` on macOS) from a compiled native executable, a prepared
server runtime, and an unpacked Node distribution for the same platform and architecture:

```sh
node apps/desktop-gpui/scripts/bundle.mjs \
  --desktop apps/desktop-gpui/target/release/t3-desktop \
  --server-root /path/to/prepared-server \
  --node-root /path/to/node-distribution \
  --output /path/to/new/T3-Code
```

The prepared server tree contains `apps/server/dist` from the existing server build, including
`client/index.html` for remote clients, and a physical production `node_modules` tree. Workspace
symlinks are rejected; use a standalone production install for the target platform rather than the
checkout's dependencies. Only those runtime directories are copied, excluding checkout state and
configuration. The Node distribution must include its `LICENSE`. Existing outputs are never replaced.
On Windows, pass the `.exe` as `--desktop`; on macOS, use an output ending in `.app`. The version
defaults to the native crate's version and can be set with `--version`.

This assembles resources; it does not build the server, fetch Node, sign an app, or generate installers.
Run its focused checks with `node --test apps/desktop-gpui/scripts/bundle.test.mjs`.

The `t3-host` integration test also starts the real server, with its data under `target/`.
