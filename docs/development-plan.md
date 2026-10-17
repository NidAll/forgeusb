# Production development ledger

Started 2026-10-17. This is an engineering task list, with evidence gates before
any compatibility claim. All destructive validation uses disposable ordinary
files or isolated virtual machines. No physical write is authorized.

- [x] Audit current source, provenance, instructions and implementation limits.
- [x] Isolate development from the authoritative checkout and divide bounded work.
- [x] Add unprivileged Windows ISO/tree preparation into verified FAT32 media.
- [x] Exercise genuine small WIM capture/split/apply, ISO extraction and failure paths.
- [x] Add working partitioned FAT32/NTFS ordinary-image formatting.
- [x] Integrate Windows preparation into CLI and compact native GUI before review.
- [x] Audit device admission, source retention, parsers and target signature handling.
- [x] Boot original BIOS/UEFI fixtures through the actual prepared/written images.
- [x] Revalidate small official Alpine media under SeaBIOS and OVMF.
- [x] Improve portable packaging, dependency reporting and CI coverage.
- [x] Run format, strict Clippy,75 workspace tests, release build, integration and GUI QA.
- [x] Reconcile user-facing support claims, limitations and release blockers.
- [ ] Integrate verified changes into the authoritative checkout and update lab records.

Windows-shaped fixtures prove image construction, file integrity and firmware
handoff only. Windows Setup, genuine installation media, physical firmware and
Secure Boot remain separate acceptance gates. CachyOS and other large distributions
must not be downloaded for this work. Source-supported raw ISOHybrid preservation
does not imply physical compatibility with every firmware.
