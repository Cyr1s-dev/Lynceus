# Third-Party Notice

## Vendored: `ctf-*` + `solve-challenge`

These skill directories are vendored from
**[ljagiello/ctf-skills](https://github.com/ljagiello/ctf-skills)**.

- **Upstream license:** MIT — full text in [`LICENSE`](./LICENSE) alongside this notice.
- **Copyright:** © 2026 Lukasz Jagiello.
- **Modifications:** Vendored verbatim for use as Lynceus agent skills; the original MIT
  copyright and permission notice are preserved as required by the MIT License.

These skills are distributed as part of this AGPL-3.0 project. The MIT license of the
vendored skills is compatible with, and preserved within, the AGPL-3.0 whole.

## First-party: `api-recon`

`api-recon/` is authored for this project and is covered by the repository's own
AGPL-3.0 license. It carries no third-party notice.

### Runtime prerequisites

The skill's manual instructs the worker to run the helpers under `api-recon/scripts/`:

- **Node** with `npm install` executed inside `api-recon/scripts/` (the only dependency is
  `puppeteer-core`, pinned by the committed `package-lock.json`).
- **Python 3** for the `.py` harvesters (`harvest_static.py`, `spider_mpa.py`,
  `extract_route_map.py`, `build_perm_tree.py`).

Without these the skill still loads as a procedure manual — it just cannot execute its
own harvesters.

