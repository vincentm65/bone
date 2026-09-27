# bone-android

Bone on Android: the desktop app's own UI (`bone_desktop::DesktopApp`), attached
to the daemon on your computer. The phone runs no agent; the computer's daemon
runs everything.

The app logs in over SSH with its own key (built in, via `russh`) and runs
`bone stdio` on the computer, which bridges to the loopback daemon (starting
one if needed). The daemon never listens beyond loopback.

## What this crate adds

- `src/ssh.rs`: the app's ed25519 key, trust-on-first-use host key pinning,
  and the `bone stdio` exec channel.
- `src/lib.rs`: the connector handed to `DesktopApp::remote`, a connect screen
  shown until a link is up (and again, with the reason, if it fails), and
  `android_main`.

Everything else is `native/`. On narrow screens its sidebar starts collapsed
and opens full-window from the ☰ button. The app runs full screen (no Android
status bar). This crate is a workspace member but not a default member, so
root `cargo build` / `cargo test` skip it.

## Toolchain (user-level, in `~/Android`)

JDK 17, Android SDK (platform 34, build-tools 34, platform-tools), NDK r27,
the `aarch64-linux-android` Rust target, and `cargo-apk`. Each shell needs:

```sh
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/27.3.13750724
export JAVA_HOME=$HOME/Android/jdk PATH=$HOME/Android/jdk/bin:$ANDROID_HOME/platform-tools:$PATH
```

## Build and install

```sh
# Release build, signed with the local debug key (fine for your own phone).
CARGO_APK_RELEASE_KEYSTORE=$HOME/.android/debug.keystore \
CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android \
  cargo apk build -p bone-android --lib --release

adb install -r target/release/apk/bone.apk
```

Updates signed with the same key keep the app's SSH key and pinned hosts.

## First connection

1. Open Bone. The connect screen shows this app's public key; tap **Copy key**
   and append it to `~/.ssh/authorized_keys` on the computer.
2. Enter `user@host` (the computer's LAN or Tailscale address) and the path to
   `bone` there if it is not on the PATH that SSH commands see (for example
   `/home/you/.local/bin/bone`).
3. Tap **Connect**. The computer's host key is pinned on first use; a changed
   key is refused.

## Desktop preview

The same code in a window, for work without a device. Its key and pinned hosts
live in `~/.local/share/bone-android`.

```sh
cargo run -p bone-android --bin bone-android-preview
```

## Not yet verified on a device

On-screen keyboard (it may cover the input), clipboard (**Copy key**), and
touch scrolling are the known risks of egui on Android.
