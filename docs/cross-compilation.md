# Cross-Compilation Guide

## Target Triples

| Platform       | Architecture | Rust Target Triple              | NPM Package                    |
|----------------|-------------|----------------------------------|--------------------------------|
| Linux          | x64         | `x86_64-unknown-linux-gnu`       | `@goodfoot/git-mesh-linux-x64`    |
| Linux          | arm64       | `aarch64-unknown-linux-gnu`      | `@goodfoot/git-mesh-linux-arm64`  |
| macOS          | x64         | `x86_64-apple-darwin`            | `@goodfoot/git-mesh-darwin-x64`   |
| macOS          | arm64       | `aarch64-apple-darwin`           | `@goodfoot/git-mesh-darwin-arm64` |
| Windows        | x64         | `x86_64-pc-windows-msvc`        | `@goodfoot/git-mesh-win32-x64`    |

## Platform-Specific Crate Backends

### notify (filesystem watcher)

The `notify` crate automatically selects the correct backend at compile time based on the target platform. No feature flags or conditional compilation are needed in our code.

| Platform | Backend                | Notes                                      |
|----------|------------------------|--------------------------------------------|
| Linux    | inotify                | Uses the Linux inotify API                 |
| macOS    | kqueue (FSEvents)      | Uses the macOS kqueue/FSEvents API         |
| Windows  | ReadDirectoryChangesW  | Uses the Win32 ReadDirectoryChangesW API   |

These backends are selected via `cfg` attributes within the `notify` crate itself. Each backend compiles only on its target platform, so there are no unused native dependencies on any given target.

### syntect (syntax highlighting)

As of Task #17, syntect is configured with pure-Rust regex support:

```toml
syntect = { version = "5", default-features = false, features = ["default-fancy"] }
```

The `default-fancy` feature enables all default syntect functionality but replaces the `onig` (Oniguruma C library) regex backend with `fancy-regex` (pure Rust). This eliminates the need for a C compiler and the `libonig` development headers during cross-compilation.

### gix (git operations)

The `gix` dependency uses `default-features = false` with `sha1` feature, which uses a pure-Rust SHA-1 implementation. No native C dependencies.

## Remaining Native Dependencies

### Linux cross-compilation

- **C cross-compilation toolchain**: Required for the `ring` crate (used transitively) and libc linking.
  - For `x86_64-unknown-linux-gnu`: `gcc` / standard build tools
  - For `aarch64-unknown-linux-gnu`: `gcc-aarch64-linux-gnu` / `g++-aarch64-linux-gnu`

### macOS cross-compilation

- **Xcode Command Line Tools**: Required on macOS runners.
- **macOS SDK**: Both `x86_64-apple-darwin` and `aarch64-apple-darwin` can be built natively on Apple Silicon Macs using `--target`.

### Windows cross-compilation

- **MSVC build tools**: Required for `x86_64-pc-windows-msvc`. Use a Windows runner or install the Visual Studio Build Tools.

## Recommended CI Matrix Configuration

```yaml
jobs:
  build:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            npm-pkg: git-mesh-linux-x64
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest
            npm-pkg: git-mesh-linux-arm64
            cross: true
          - target: x86_64-apple-darwin
            os: macos-latest
            npm-pkg: git-mesh-darwin-x64
          - target: aarch64-apple-darwin
            os: macos-latest
            npm-pkg: git-mesh-darwin-arm64
          - target: x86_64-pc-windows-msvc
            os: windows-latest
            npm-pkg: git-mesh-win32-x64

    runs-on: ${{ matrix.os }}

    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - name: Install cross-compilation tools (Linux ARM64)
        if: matrix.cross
        run: |
          sudo apt-get update
          sudo apt-get install -y gcc-aarch64-linux-gnu g++-aarch64-linux-gnu

      - name: Build
        working-directory: packages/git-mesh
        run: cargo build --release --target ${{ matrix.target }}
        env:
          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER: aarch64-linux-gnu-gcc

      - name: Package binary
        run: |
          BINARY_NAME=git-mesh
          if [ "${{ matrix.os }}" = "windows-latest" ]; then
            BINARY_NAME=git-mesh.exe
          fi
          mkdir -p npm/${{ matrix.npm-pkg }}/bin
          cp target/${{ matrix.target }}/release/$BINARY_NAME npm/${{ matrix.npm-pkg }}/bin/
```

For the Linux ARM64 build, the `cross` tool (https://github.com/cross-rs/cross) is an alternative that uses Docker containers with pre-configured toolchains, avoiding manual apt package installation.
