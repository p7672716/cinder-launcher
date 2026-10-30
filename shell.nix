{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ pkg-config cmake clang gcc rustfmt fontconfig libxkbcommon libx11 libxcb wayland openssl zstd alsa-lib vulkan-loader mesa xwininfo xdotool ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ fontconfig libxkbcommon libx11 libxcb wayland openssl zstd alsa-lib vulkan-loader ]);
  # Reuse an existing Rust toolchain when it is installed without a rustup shim.
  shellHook = ''
    rust_bin="$HOME/.rustup/toolchains/stable-${pkgs.stdenv.hostPlatform.config}/bin"
    if [ -x "$rust_bin/cargo" ] && [ -x "$rust_bin/rustc" ]; then
      export PATH="$rust_bin:$PATH"
    fi
  '';
}
