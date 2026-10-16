# Build and package ForgeUSB

Use Rust stable >= 1.88, a Linux desktop session, a C linker, util-linux, fontconfig and libxkbcommon. Polkit and an active authentication agent are needed for installed physical-device writes. The GUI stays an ordinary user process. Do not install system packages automatically as part of testing.

From the project root:

```sh
cargo build --release --locked
cargo test --workspace --locked
./target/release/forgeusb check-dependencies
./target/release/forgeusb-gui
```

The code uses fixed `/usr/bin` utility paths and `/usr/lib/forgeusb/forgeusb-helper`. A nonstandard prefix requires coordinated changes to the helper invocation and Polkit policy; changing `PREFIX` alone will not make elevation work. No setuid binary is installed. Neither source builds nor packaging tests authorize physical writes.

## Arch / CachyOS

The local source-archive recipe generates an exact checksum and excludes upstream reference artifacts. This prepares a reviewable native package without installing it:

```sh
bash packaging/make-source-archive.sh
cd dist
makepkg
```

Review the package contents and policy before a separate administrator-approved installation with your package manager. `makepkg` dependency installation flags are intentionally absent. Build-time Cargo may need network access to acquire the locked crates. Distribution build services should vendor the resolved crate sources and use an offline build.

The source archive is deterministic for identical saved inputs and `SOURCE_DATE_EPOCH`; this is not a claim that complete Rust release binaries have been independently reproduced. Supply-chain metadata, all transitive notices, a release signature and clean-host package validation are still release gates.

## Unprivileged installation staging

```sh
make -f packaging/Makefile install DESTDIR=/tmp/forgeusb-package-stage
desktop-file-validate /tmp/forgeusb-package-stage/usr/share/applications/forgeusb.desktop
```

The staged helper/policy do not become active merely by copying them to `/tmp`. A real installation changes privileged `/usr` paths and is an explicit user action. The helper must be root-owned, non-writable by ordinary users, and must retain its independent request validation. Do not run `forgeusb-gui` with sudo.

## Debian / Ubuntu and Fedora

`packaging/debian/` and `packaging/fedora/forgeusb.spec` are starting recipes, not tested distribution packages. Debian needs proper maintainer metadata, changelog/source format and a vendored Cargo build; use a supported release with rustc >= 1.88. Copy the Debian directory into a clean source tree as `debian/`, vendor/check licenses and build with `dpkg-buildpackage` in a disposable build environment. Polkit package naming differs between releases and needs verification.

For Fedora, place the generated source archive in the RPM source directory, provide a reviewed vendored crate source or distribution Rust macros, then run `rpmbuild -ba packaging/fedora/forgeusb.spec` in a disposable build environment. The recipe deliberately builds offline: it will not succeed until Cargo dependencies are supplied. No DEB or RPM installation was performed during development.

## Portable distribution

The release binary is locally portable only where its dynamic C/desktop dependencies are available. An AppImage/Flatpak is not produced. Raw USB privilege mediation and access to the system helper need a carefully reviewed host boundary, so a GUI bundle alone is insufficient. No claim of a single universal Linux binary is made.
