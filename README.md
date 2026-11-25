# GlobalPTT

GlobalPTT is a Linux push-to-talk audio bridge. It creates a virtual
microphone with PipeWire and uses the desktop Global Shortcuts portal to
control mute state. Select **GlobalPTT Virtual Microphone** as the input device
in the app where you want to use push-to-talk.

## Build and run

With Nix:

```sh
nix develop
cargo run --release
```

Or install Rust, `pkg-config`, and the PipeWire development files, then run:

```sh
cargo build --release
./target/release/GlobalPTT
```

Approve the shortcut request from the desktop portal. Hold the configured
shortcut to unmute; releasing it mutes the virtual microphone. Set
`GLOBALPTT_MIC` to a PipeWire object name to choose the captured source.
Short confirmation tones play when push-to-talk is enabled and disabled.

## License

This project is released into the public domain under the terms of the
[Unlicense](LICENSE).
