# bone-android

Phone client for a Bone daemon running on another machine. It is a thin
frontend: the daemon (on your computer) runs the agent, tools, and history; the
phone only sends prompts and approvals and shows the conversation.

The connection is `ssh <host> -- bone stdio`. SSH handles login and encryption,
and `bone stdio` on the computer bridges to its loopback daemon (starting one if
needed). The daemon never listens beyond loopback.

## Isolation

This crate shares only `bone-protocol` (the messages) and `bone-client` (the
transport). It has its own small event reducer (`src/state.rs`) and touch UI
(`src/app.rs`); nothing in `core`, `tui`, or `native` depends on it. It is a
workspace member but not a default member, so `cargo build` / `cargo test` at
the root skip it.

## Desktop preview

The same code runs in a phone-sized window, so most UI work needs no device:

```sh
cargo run -p bone-android --bin bone-android-preview -- --ssh devbox
cargo run -p bone-android --bin bone-android-preview -- --connect 127.0.0.1:7878
```

With no arguments it opens the connect screen.

## Remote machine requirements

- `bone` installed and on the `PATH` that SSH commands see (non-interactive
  shells), or set `BONE_SSH_REMOTE_BIN=/path/to/bone`.
- Key or agent authentication (the client uses `BatchMode=yes`).

## Status

- [x] Preview window: connect over SSH, chat list drawer, one conversation,
      streaming replies, tool rows, approvals, Send/Stop.
- [ ] In-app SSH (`russh`) to replace the `ssh` program, with key import and
      trusted-host storage — required on Android, which has no `ssh`.
- [ ] Android entry point (`android_main`), eframe Android support, and an APK
      build via the NDK (`cargo ndk`).
- [ ] Verify the on-screen keyboard and touch input on a real device.
