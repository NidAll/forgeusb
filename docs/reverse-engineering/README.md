# Reverse-engineering evidence index

Study target: Rufus v4.15 (build 2396), commit `6d8fbf98305ff37eb531c45cbd6ff44563c53917`. Source was pinned to the release matching the official executable rather than conflating current master with shipped behavior.

- [Source architecture and subsystem mapping](source-architecture.md): actual filenames, entrypoints, dependencies, threads, image-to-device path, boot/filesystem/Windows mechanisms and Linux translations.
- [Linux design and diagrams](linux-design.md): component/privilege architecture and dependency choices.
- [Binary study](binary-study.md): provenance, UPX derivation, Ghidra evidence, source/binary function mappings and unverified areas.
- [Reference lock](reference-lock.json): exact commits, URLs, dates and license observations for all four upstream projects.
- [Complete tracked Rufus tree](rufus-v4.15-tracked-files.txt) and [tree counts](tree-counts.json).
- [Packed artifact REA evidence](rufus-packed-evidence.json) and [unpacked artifact REA evidence](rufus-unpacked-evidence.json): original retained static evidence, complete pseudocode/assembly for selected functions and provider limitations. Local paths in historical records identify the workspace at analysis time.

The application is an independent Rust implementation. Source analysis establishes mechanisms; static binary analysis corroborates selected relationships. Neither establishes Windows runtime behavior, physical USB boot success, Secure Boot acceptance, or behavior of functions not examined. See ../compatibility.md and ../validation.md for product evidence limits.
