Name:           forgeusb
Version:        0.1.0
Release:        1%{?dist}
Summary:        Native Linux USB image writer
License:        GPL-3.0-or-later
Source0:        forgeusb-%{version}.tar.gz
BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  make
BuildRequires:  gcc
Requires:       util-linux
Requires:       polkit
Requires:       libxkbcommon
Requires:       fontconfig
Suggests:       wimlib-utils
Suggests:       kdialog
Suggests:       zenity

%description
Inspect, plan, write and verify raw images with explicit target confirmation.
This development build does not create Windows installation media.

%prep
%autosetup

%build
cargo build --release --locked --offline

%check
cargo test --workspace --locked --offline

%install
make -f packaging/Makefile install DESTDIR=%{buildroot}

%files
%license LICENSE THIRD_PARTY_NOTICES.md
%{_bindir}/forgeusb
%{_bindir}/forgeusb-gui
/usr/lib/forgeusb/forgeusb-helper
%{_datadir}/applications/forgeusb.desktop
%{_datadir}/icons/hicolor/scalable/apps/forgeusb.svg
%{_datadir}/icons/hicolor/*x*/apps/forgeusb.png
%{_datadir}/polkit-1/actions/io.forgeusb.write.policy
/usr/share/licenses/forgeusb
