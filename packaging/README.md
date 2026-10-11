# Packaging inputs

`scripts/package.sh` and `scripts/release-gate.sh` read these files.

* `MAINTAINER` — one line, `Name <address@domain>`: the `.deb` `Maintainer`
  field, the RPM `Packager` tag and the `Upstream-Contact` of the `.deb`
  `copyright` file. Packaging refuses to run without it; the release gate
  also rejects placeholders.
* `notices/STATIC-RUNTIME-NOTICES.txt` — notices for the statically linked C
  runtime of the musl release binaries, generated with provenance by
  `scripts/fetch-runtime-notices.sh` for the pinned release toolchain. Every
  file in `notices/` is shipped in all packages.

The project license texts (`LICENSE-MIT`, `LICENSE-APACHE`) and the license
expression (`license` in `Cargo.toml`) live at the repository root and are
shipped in every artifact. See [RELEASING.md](../RELEASING.md).
