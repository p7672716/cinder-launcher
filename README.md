# Cinder Launcher

A Rust desktop launcher under development for managing user-created Minecraft Java Edition instances and mod environments. The desktop UI is built with GPUI and GPUI Kit.

**Development status:** Alpha/prototype. Not ready for general use. This is an independent project, not an official Microsoft or Mojang application.

## Implemented foundations

- Instance creation, selection, renaming and isolated game directories.
- Official Minecraft version metadata, downloads of client files, assets, libraries and Mojang-provided Java runtimes, with integrity verification.
- Vanilla launch planning, Java selection, process management and token redaction in captured logs.
- Microsoft OAuth 2.0 authorization-code flow with PKCE and loopback callback; Xbox Live/XSTS and Minecraft Services ownership/profile request logic.
- Local configuration of an Entra application client ID and optional service API credentials; on Windows, sensitive credentials are stored through Windows Credential Manager.

**Known limitation:** End-to-end Microsoft/Minecraft authentication and reaching the Minecraft title screen have **not been verified**. During testing the authentication flow displayed an HTTP 403; the rejecting endpoint has not yet been conclusively identified. Minecraft Services application access may require additional approval. Minecraft account passwords are not collected or stored. Support for NeoForge game launches, automatic mod management and complete product UI remains unfinished.

## Build and test

Use Rust 1.99.1 (see `rust-toolchain.toml`) with the native build dependencies required by GPUI/GPUI Kit. On Linux, `nix-shell shell.nix` supplies native build dependencies. On Windows, a Rust MSVC toolchain, Visual Studio C++ Build Tools / Windows SDK, and CMake are required.

```sh
cargo check --all-targets --locked
cargo test --locked
cargo build --locked
cargo run --locked
```

The GUI is the default Cargo executable (`cinder-launcher`). The supplementary `cinder-data` command is intended for development and testing of Minecraft file downloads:

```sh
cargo run --locked --bin cinder-data -- inspect 1.21.1
```

The repository contains source and synthetic test fixtures only. It does not bundle Minecraft game files, user accounts, real mods or authentication secrets. Minecraft game content is obtained from official sources when the user performs a relevant operation.

## Development and licensing

Cinder Launcher is in active development. Public source availability is for review; an open-source license has **not yet been chosen**. No reuse rights are granted simply by making the repository public.
