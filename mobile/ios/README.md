# Po for iOS (to build)

The iOS app is not written yet (no Mac was available). Everything it needs below the interface is
already shared with Android: `crates/po-mobile` is the whole phone core (GitHub sign-in, the
account's PCs, the links, the sessions followed with replay after a cut), with no Android-specific
code, exposed through UniFFI.

## Building the core

On a Mac with Xcode and Rust:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
# Add "staticlib" to crate-type in crates/po-mobile/Cargo.toml, then:
cargo build -p po-mobile --release --target aarch64-apple-ios
cargo build -p po-mobile --release --target aarch64-apple-ios-sim
# Swift bindings, from a build that keeps UniFFI's metadata (not stripped):
cargo build -p po-mobile --target aarch64-apple-ios-sim
cargo run -p po-mobile --features bindgen --bin uniffi-bindgen -- generate \
  --library target/aarch64-apple-ios-sim/debug/libpo_mobile.a --language swift --out-dir mobile/ios/Generated
# One framework for device and simulator:
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libpo_mobile.a -headers mobile/ios/Generated \
  -library target/aarch64-apple-ios-sim/release/libpo_mobile.a -headers mobile/ios/Generated \
  -output mobile/ios/PoMobile.xcframework
```

`ring` (TLS) builds for iOS with Xcode's clang; nothing else needs a C toolchain.

## The app

A SwiftUI app mirroring `mobile/android/app/src/main/java/dev/po/remote`:

- `PoCore(dataDir:deviceName:githubApi:githubWeb:extraHosts:)` with the app's Application Support
  folder; calls block, so make them off the main actor (`Task.detached`).
- Implement the `Listener` protocol and hop to the main actor in each callback; `on_session` gives
  the rows from `from` to the end: drop the shown rows from `from`, append the new ones.
- Screens: sign-in (device code, open the URL, `signInWait`), PCs and projects, sessions of a
  project, conversation (rows by `ItemKind`, tool rows with their diff, folded reasoning), input
  bar with the model picker (`/model <id>`) and dictation (`SFSpeechRecognizer`), approval and
  question sheets, command menus (`commands`, `menu`), settings.
- Local network: add `NSLocalNetworkUsageDescription` and `NSBonjourServices`
  (`_po-link._udp`) to Info.plist for the mDNS discovery.
- Background: iOS does not keep a socket open in the background; a turn finishing while the app
  is suspended is seen when it comes back (the events are replayed). Notifications while in the
  background would need a push service, which Po deliberately does not use.
- Network changes: `NWPathMonitor` → `networkChanged()`.
