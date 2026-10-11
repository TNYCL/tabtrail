# Packaging inputs

`scripts/package.sh` and `scripts/release-gate.sh` read these files.

* `MAINTAINER` (**owner input**, not yet present) — one line,
  `Name <contact>`, for the `.deb` Maintainer field. Publication is blocked
  until it exists.
* `notices/STATIC-RUNTIME-NOTICES.txt` — notices for the statically linked C
  runtime of the musl release binaries, generated with provenance by
  `scripts/fetch-runtime-notices.sh` for the pinned release toolchain. Every
  file in `notices/` is shipped in all packages.

See [RELEASING.md](../RELEASING.md).
