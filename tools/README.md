# tools/

Workspace-local home for the external security tools Lynceus drives (nuclei,
katana, jsluice, httpx, ffuf, dalfox, semgrep, ...). Binaries here are **not**
committed — only this file and `.gitignore` are tracked.

## Configure

The provisioner reads the install recipes in
`resources/tool-catalog/curated_tools.yaml`, installs each tool into this
directory, and records the resolved paths in
`data/config/local-tools.json` so the runtime tool gate and domain adapters
can find them.

The allowlisted recipes are available in the web console under **Engine
Pool**. Selecting **Install** starts a Rust background job; the page polls its
status and refreshes executable detection when it completes. Manual-only
recipes show upstream installation guidance instead of executing arbitrary
commands.

Layout after provisioning:

- `tools/bin/` — verified release, Go, and Rust binaries.
- `tools/pyenv/` — isolated virtualenv for pip-installed CLIs (semgrep, crlfsuite).
- `tools/go/` — isolated Go module and build caches.

## Toolchains

Automatic install needs the matching toolchain on `PATH`:

- **github_release** — downloads only from the repository declared by the
  tool's official `upstream_url`. Every recipe pins a release tag and a
  per-platform SHA-256; a mismatch aborts before the executable is written.
- **go** — jsluice is built from a full official-repository commit SHA with
  Go checksum-database verification. Mutable refs such as `latest`, `main`,
  and `master` are rejected.
- **pip** — semgrep and crlfsuite use exact versions whose package metadata
  points back to their official GitHub repositories. They are installed into
  the isolated `tools/pyenv` environment.
- **cargo** — supported for custom/future recipes; feroxbuster currently uses
  its official prebuilt release.

Automatic recipes are allowlisted and must match the catalog's official GitHub
repository. Installed version, executable SHA-256, release archive SHA-256,
asset URL, and integrity status are recorded in
`data/config/local-tools.json`. Existing binaries are reused only when
their recorded version and hash still match; version updates require an
explicit reviewed catalog change.

Jsluice requires CGO. On Windows the provisioner can use a workspace-local
LLVM-MinGW compiler under `tools/toolchains/`; this workstation uses the
official `mstorsjo/llvm-mingw` `20260616` UCRT release, with its source asset
and SHA-256 recorded beside the extracted toolchain. The compiler and manifest
remain excluded from source control with the rest of `tools/`.

Tools marked `manual` (currently only `wappalyzergo`, which upstream ships as a
Go library rather than a prebuilt release) show upstream instructions in Engine
Pool. Install them yourself and configure the executable path there.

## Overrides

- `LYNCEUS_TOOLS_DIR` — install into a different directory.
- `LYNCEUS_LOCAL_TOOLS_CONFIG` — store detected executable paths in a different
  JSON file.

## Publishing

`tools/.gitignore` excludes every provisioned binary and virtual environment,
while keeping this README and the ignore rule. The generated
`data/config/local-tools.json` is also ignored at the repository root.
Published source therefore contains recipes only; each user explicitly chooses
which tools to install.
