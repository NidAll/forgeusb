# Build and package ForgeUSB

Use Rust stable >= 1.88, a Linux desktop session, a C linker, util-linux, fontconfig and libxkbcommon. Install `kdialog` (KDE) or `zenity` (fallback) for the native SELECT image chooser; pasting a path and pressing Enter works without either. Polkit and an active authentication agent are needed for installed physical-device writes. The GUI stays an ordinary user process. Do not install system packages automatically as part of testing.

From the project root:

```sh
cargo build --release --locked
cargo test --workspace --locked
./target/release/forgeusb check-dependencies
./target/release/forgeusb-gui
```

The code uses fixed `/usr/bin` utility paths and `/usr/lib/forgeusb/forgeusb-helper`. A nonstandard prefix requires coordinated changes to the helper invocation and Polkit policy; changing `PREFIX` alone will not make elevation work. No setuid binary is installed. Neither source builds nor packaging tests authorize physical writes.

## Install from this checkout

Run the installer as your desktop user. The default system mode builds the locked
release binaries without root, then requests administrator authentication through
Polkit (or sudo if pkexec is unavailable) for a fixed list of `/usr` files:

```sh
bash packaging/install.sh --system
# Reuse an already verified release build:
bash packaging/install.sh --system --no-build
```

Python 3 is required by the installer. It installs the CLI and GUI in `/usr/bin`,
the narrow helper in `/usr/lib/forgeusb`, its Polkit policy, desktop entry, original
SVG plus seven PNG icon sizes, and license notices. The embedded window icon and
`forgeusb` Wayland app ID / X11 class match the desktop launcher. Desktop caches
are refreshed when their system utilities are present. Launch **ForgeUSB** from
the application menu or run `forgeusb-gui` as your normal user.

The installer never installs OS dependencies, starts a storage write, formats a
partition, or grants passwordless access. Staging inputs are read with the caller's
credentials before the elevated worker copies bytes to trusted root-owned
locations. Each installed file is replaced atomically with explicit modes; there
is no whole-install transaction or automatic rollback. Existing symlink targets,
untrusted system directories and pacman-owned destination files are refused.
Upgrade a package-managed installation through its package manager instead.
An interrupted installation may require rerunning the installer.

An administrator-free desktop/CLI installation is also available:

```sh
bash packaging/install.sh --user
```

This installs into `~/.local`, uses an absolute GUI launcher path, and does not
install or authorize the privileged helper. Physical writing still needs the
system helper installation. Add `~/.local/bin` to your shell PATH for CLI use.

For packaging/tests, stage the complete system layout in a **new** directory:

```sh
bash packaging/install.sh --no-build --stage /tmp/forgeusb-NEW-stage
python3 tests/installation.py
```

The installer records file hashes in `share/forgeusb/install-receipt.json` under
its prefix. Remove only the matching unmodified installation with:

```sh
bash packaging/install.sh --system --uninstall
# Or, for an installation made using --user:
bash packaging/install.sh --user --uninstall
```

Uninstallation refuses changed files or an unrecognized receipt; it does not
recursively remove directories or touch your source images. Installations made
using make/package-manager recipes have their own removal procedure and do not
have this installer receipt. Cache/menu updates may take a desktop refresh after
uninstallation.

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
