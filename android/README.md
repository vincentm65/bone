# bone-android

Phone client for a Bone daemon running on your computer. The daemon runs the
agent, tools, and history; the phone sends prompts and approvals and shows the
conversation.

The app logs in over SSH with its own key (built in, via `russh`) and runs
`bone stdio` on the computer, which bridges to the loopback daemon (starting
one if needed). The daemon never listens beyond loopback.

## Isolation

Shares only `bone-protocol` (messages) and `bone-client` (transport); keeps its
own reducer (`src/state.rs`), SSH (`src/ssh.rs`), and touch UI (`src/app.rs`).
A workspace member but not a default member, so root `cargo build` / `cargo
test` skip it.

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

Phone setup: Settings → About phone → tap Build number 7 times, then Developer
options → USB debugging; plug in and accept the prompt.

## First connection

1. Open Bone. The connect screen shows this app's public key; tap **Copy key**
   and append it to `~/.ssh/authorized_keys` on the computer.
2. Enter `user@host` (the computer's LAN or Tailscale address) and the path to
   `bone` there if it is not on the PATH that SSH commands see (for example
   `/home/you/.cargo/bin/bone`).
3. Tap **Connect**. The computer's host key is pinned on first use; a changed
   key is refused.

## Desktop preview

The same code in a phone-sized window, for UI work without a device. Its key
and pinned hosts live in `~/.local/share/bone-android`.

```sh
cargo run -p bone-android --bin bone-android-preview -- --ssh me@host [--bone /path/to/bone]
cargo run -p bone-android --bin bone-android-preview -- --connect 127.0.0.1:7878
```

## Not yet verified on a device

On-screen keyboard behaviour, clipboard (**Copy key**), and touch scrolling are
the known risks of egui on Android.
