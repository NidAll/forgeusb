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
BuildRequires:  pkgconf-pkg-config
BuildRequires:  python3
Requires:       util-linux
Requires:       polkit
Requires:       libxkbcommon
Requires:       fontconfig
Recommends:     dosfstools
Recommends:     mtools
Recommends:     7zip
Recommends:     wimlib-utils
Recommends:     ntfsprogs
Suggests:       kdialog
Suggests:       zenity

%description
Inspect, plan, write and verify raw images with explicit target confirmation.
Prepare FAT32 Windows UEFI media from installation trees or optical ISOs.
WIM splitting avoids FAT32 file-size limits; no Microsoft files are bundled.

%prep
%autosetup

%build
python3 packaging/build.py build --release --locked --offline

%check
python3 packaging/build.py test --workspace --locked --offline

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
