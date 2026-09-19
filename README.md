# licenselint

Reports every dependency's license and flags the ones your policy
forbids, across `Cargo.toml`, `package-lock.json` and `requirements.txt`
**in one pass**. `license-checker` (Node) and `pip-licenses` (Python)
each do exactly one ecosystem, and `cargo-license` does the third; a
polyglot repo — this workspace's own is Rust CLI tools plus Node/Express
services plus Expo apps plus a few Python model services — needs three
separate tools, three output formats, and three copies of the same
allowlist today.

## Usage

```bash
licenselint .                                        # walk a repo, find every manifest
licenselint tools/dsnmask --deny GPL-3.0 --deny AGPL-3.0
licenselint . --allow MIT --allow Apache-2.0 --allow ISC --violations-only
licenselint Cargo.toml app/package-lock.json ai/requirements.txt   # any mix, one report
licenselint . --config licenselint.json --json
```

Exit code `1` if anything violates the policy, `0` if not, `2` on a
usage/IO error. `--violations-only` drops the `ok` lines for CI while the
footer still reports everything that was scanned, so a quiet report never
hides how much it actually looked at.

The config file replaces (or merges with) the flags:

```json
{
  "allow": ["MIT", "Apache-2.0", "ISC", "BSD-3-Clause"],
  "deny": ["GPL-3.0", "AGPL-3.0", "MPL-2.0"],
  "pip_licenses": { "torch": "BSD-3-Clause", "fastapi": "MIT" },
  "fail_unknown": false
}
```

## Licenses are expressions, not strings

Real manifests give you `MIT OR Apache-2.0`, `Apache-2.0 AND ISC`,
`(MIT OR Apache-2.0) AND Unicode-3.0`, Cargo's legacy `MIT/Apache-2.0`,
and `Apache-2.0 WITH LLVM-exception` — all four of which are live in this
monorepo's own dependency trees. So the expression is parsed and
allow/deny are evaluated over its boolean structure:

- an **allowlist** is satisfied if *some* branch of every `OR` is allowed
  and *every* branch of every `AND` is. `MIT OR Apache-2.0` passes an
  Apache-only allowlist; `(MIT OR Apache-2.0) AND Unicode-3.0` does not
  pass a MIT+Apache one, because the `AND` half still binds you.
- a **denylist** fires only when the dependency is *unavoidably*
  denied — the exact dual. `MIT OR GPL-3.0` under `--deny GPL-3.0` is
  **not** flagged, because you can simply take the MIT branch;
  `MIT AND GPL-3.0` is.
- ids are compared case-insensitively with `+`, `-only` and `-or-later`
  normalised away, so `--deny GPL-3.0` catches `GPL-3.0`, `GPL-3.0+`,
  `GPL-3.0-only` and `GPL-3.0-or-later`, and — unlike a substring
  match — catches neither `LGPL-3.0` nor `AGPL-3.0`. The flip side is
  that denying `GPL-3.0-only` while allowing `GPL-3.0-or-later` is
  deliberately not expressible.

A license string that is not a parseable expression (`BSD-like`,
`SEE LICENSE IN LICENSE.md` — both real values found in this repo's
lockfiles) is reported as such rather than silently passing.

## What each ecosystem's detection actually does

**Cargo — authoritative, offline once the registry cache is warm.** Runs
`cargo metadata --format-version 1` on each manifest and reads the
`license` field every crates.io crate carries in its own `Cargo.toml`.
Nothing is inferred. The project's own workspace members are excluded;
everything else in the resolved graph is kept, which deliberately
includes dev-dependencies, build-dependencies and other platforms'
dependencies (`windows-sys` shows up on Linux), because that is what
`cargo metadata` returns unfiltered and over-reporting is the safe
direction for a license review. A crate that sets only `license-file` is
reported as unresolved, not labelled with its filename.

**npm — authoritative in the common case, best-effort otherwise.** Reads
the `packages` map of a lockfileVersion 2/3 `package-lock.json`; modern
npm writes a `license` field into each entry, so this is usually offline
and exact. It is *not* guaranteed to be there — of the four real
lockfiles in this repo, one has a license on 607/607 entries and another
on 6/388 — so entries without one fall back to reading
`node_modules/<pkg>/package.json`, which only helps if dependencies are
installed. Anything still unresolved is reported as unknown. The legacy
`{"type": "MIT"}` object and `licenses: [...]` array shapes are
understood; the `""` root entry and workspace `link` entries are skipped;
scoped and nested package names are recovered from the lockfile path.

**pip — the honest asymmetry: there is no license in the file.** Cargo
and npm ship the license *with the dependency list*. `requirements.txt`
ships names and version specifiers and nothing else, and there is no
offline, universal name→license mapping for PyPI. `pip-licenses` itself
sidesteps this by only ever reporting on an *installed* environment. So
there are two opt-in resolution paths here and no default one:

1. `--pip-show` asks the currently active Python environment via
   `pip show`, reading installed package metadata. Accurate, but covers
   only installed packages and answers for the *installed* version, not
   the pinned one. Both the old free-text `License:` field and PEP 639's
   `License-Expression:` are read, with the latter winning.
2. `pip_licenses` in the config file — a map you maintain by hand. The
   only thing that works in CI with no installed environment. Names are
   matched PEP 503-style, so `huggingface-hub` in the config matches
   `huggingface_hub` in the requirements file.

Neither is used unless you ask. A pip dependency with a blank license
column means *nothing here knows*, not *no license* — and with an
allowlist in force, unknown counts as a failure, because an unresolved
license cannot be proved to be on the list. Under a deny-only policy
unknown is reported but tolerated (`--fail-unknown` overrides), because
with pip in the mix unknowns are the normal case, not an anomaly.

## Status: built, and verified against this monorepo's real dependency trees

- **59 unit tests** (`cargo test --lib`): the SPDX expression parser and
  the allow/deny duality (including that `MIT OR GPL-3.0` is *not* a
  denylist violation while `MIT AND GPL-3.0` is, that `--deny GPL-3.0`
  catches all four `-only`/`-or-later`/`+` spellings but not `LGPL`/
  `AGPL`, and that `(MIT OR Apache-2.0) AND Unicode-3.0` fails a
  MIT+Apache allowlist); `cargo metadata` parsing against a captured
  slice of this repo's real metadata output, with the workspace member
  excluded and two coexisting `syn` versions both kept; `package-lock.json`
  parsing for scoped/nested names, the root and `link` entries, the two
  legacy license shapes, and a clear error on lockfileVersion 1;
  `requirements.txt` parsing of extras, markers, `-r`/`-e`/`--index-url`
  lines and VCS URLs; `pip show` output parsing for both field spellings;
  report formatting, sorting, the `--violations-only` footer, and the
  JSON shape. None of them shell out — the fixtures are real captured
  output, so the tests do not need a network or a Python environment.
- **Cargo, live against real crates in this repo.** `tools/dsnmask`:
  **59 dependencies**, every one resolved. Real strings extracted:
  `serde 1.0.229` → `MIT OR Apache-2.0`, `clap 4.6.6` →
  `MIT OR Apache-2.0`, `url 2.5.8` → `MIT OR Apache-2.0`,
  `memchr 2.8.3` → `Unlicense OR MIT`, `strsim 0.11.1` → `MIT`,
  `utf8parse 0.2.2` → `Apache-2.0 OR MIT`, `unicode-ident 1.0.24` →
  `(MIT OR Apache-2.0) AND Unicode-3.0`, and the whole `icu_*` family →
  `Unicode-3.0`. `tools/pgqueue`: **300 dependencies**, again all
  resolved — `tokio 1.53.1` → `MIT`, `sqlx 0.8.6` → `MIT OR Apache-2.0`,
  `ring 0.17.14` → `Apache-2.0 AND ISC`, `rustls 0.23.43` →
  `Apache-2.0 OR ISC OR MIT`, `encoding_rs 0.8.35` →
  `(Apache-2.0 OR MIT) AND BSD-3-Clause`, `webpki-roots 1.0.9` →
  `CDLA-Permissive-2.0`, `foldhash 0.2.0` → `Zlib`, and
  `zstd-sys 2.0.16+zstd.1.5.7` → `MIT/Apache-2.0`, which is where the
  legacy slash form stopped being hypothetical.
- **The whole `tools/` directory in one invocation**: 47 sibling crates,
  no workspace root, each one given its own `cargo metadata` run and then
  collapsed to **562 unique dependencies** — 0 unknown, 0 unparseable, 0
  denied under `--deny GPL-3.0 --deny AGPL-3.0`, exit `0`, in about 17s
  with a warm registry cache. Under a broad permissive allowlist
  (MIT/Apache-2.0/ISC/BSD-2/BSD-3/Unlicense/Zlib/0BSD) the same scan
  flags **25**, and two of them are findings nobody went looking for:
  `option-ext 0.2.0` is `MPL-2.0` — weak copyleft, sitting transitively
  in this repo right now — and both `webpki-roots` versions are
  `CDLA-Permissive-2.0`, a license most hand-written allowlists have
  never heard of.
- **A result worth having, from a real crate:** running `tools/dsnmask`
  under `--allow MIT --allow Apache-2.0 --allow Unlicense` — the
  allowlist most people would write first — flags **19 of its 59
  dependencies**, every one of them `Unicode-3.0` (the `icu_*` crates
  `url`'s `idna` pulls in, plus `unicode-ident`'s
  `(MIT OR Apache-2.0) AND Unicode-3.0`). That is a genuine, correct
  finding about a crate in this repo that nothing was looking for.
- **The deliberate denylist test, against a genuinely GPL crate.** Built
  a throwaway fixture crate depending on `kmon = "1.7"` — a real
  crates.io crate published under `GPL-3.0` (confirmed against the
  crates.io API first). `licenselint <fixture> --deny GPL-3.0 --deny
  AGPL-3.0` reports `DENIED  cargo  kmon 1.7.1  GPL-3.0  (denied by
  GPL-3.0)` out of **119 resolved dependencies** and exits `1`; the same
  tree with no denylist exits `0` and lists `kmon 1.7.1  GPL-3.0` as
  `ok`. So both the flag and the clean path are real.
- **npm, live against real `package-lock.json` files in this repo**
  (read-only; nothing under `apps/` was modified). `apps/qr/app`:
  **577 dependencies**, all licensed inline by npm itself —
  `react 19.2.3` → `MIT`, `typescript 6.0.3` → `Apache-2.0`,
  `semver 7.8.5` → `ISC`, `caniuse-lite` → `CC-BY-4.0`. With
  `--deny MPL-2.0`, **12 `lightningcss*` packages** are correctly flagged
  and the run exits `1`. `apps/hearttalk/ui`: 426 dependencies, and a
  permissive allowlist flags five real ones — `argparse` (`Python-2.0`),
  `caniuse-lite` (`CC-BY-4.0`), `mdn-data` (`MPL-2.0`), `minimatch`
  (`BlueOak-1.0.0`) and `css-select 1.2.0`, whose lockfile really does
  say `BSD-like`.
- **npm's gap, observed rather than assumed**: `apps/siliconbazaar/bff`
  reports 40 of 189 dependencies as unknown and `apps/atmos/ui` reports
  **375 of 381**, because those lockfiles simply have no `license` field
  on most entries and `node_modules` is not installed in this sandbox for
  the fallback to read. The tool says `unknown` for all of them instead
  of guessing.
- **pip, both halves, honestly.** Run against the real
  `apps/job-search-app/ai-tests/requirements-matcher.txt`, all **6**
  requirements (`fastapi`, `uvicorn`, `psycopg`, `torch`,
  `sentence-transformers`, `huggingface_hub`) come back `unknown` —
  **including with `--pip-show`**, because none of them are installed in
  this sandbox. That is the asymmetry working as documented, not a bug.
  To actually exercise resolution, a throwaway `requirements.txt` of
  packages that *are* installed in this machine's real Python 3.14
  environment was scanned with `--pip-show`: `requests` → `Apache-2.0`,
  `urllib3` → `MIT`, `certifi` → `MPL-2.0`, `psutil` → `BSD-3-Clause`,
  `attrs` → `MIT`, `PyGObject` → `LGPL-2.1-or-later`, and `numpy` →
  `BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0`. Under
  `deny: ["MPL-2.0", "LGPL-2.1", "GPL-2.0"]`, `certifi` and `PyGObject`
  were both flagged (the latter via the `-or-later` normalisation) and
  the run exited `1`, while `torch` — not installed — was resolved to
  `BSD-3-Clause` from the config file's `pip_licenses` map in the same
  run.
- **One real wart that test turned up, left unfixed on purpose**: the
  installed `apparmor` package reports its license as `GPL-2`, which is
  not a valid SPDX id, so a `GPL-2.0` denylist entry does **not** catch
  it. pip metadata is free text and always has been; inventing an alias
  table for every non-standard spelling people have typed into
  `setup.py` would be guessing, which is the one thing this is supposed
  not to do. It shows up in the report with its real string, `GPL-2`,
  where a human can see it.
- **The whole point, in one command**: `licenselint tools/dsnmask
  apps/qr/app/package-lock.json <pip-fixture>/requirements.txt --deny
  GPL-3.0 --deny AGPL-3.0 --deny MPL-2.0 --pip-show` — one invocation,
  one policy, one exit code — scanned **645 dependencies (cargo 59,
  npm 577, pip 9)** and flagged **13** MPL-2.0 violations across two
  different ecosystems.

**Not done / deliberately deferred**: lockfileVersion 1 npm lockfiles
(the old nested `dependencies` tree, which has no license data at all —
rejected with a clear message rather than silently reporting nothing);
Yarn/pnpm lockfiles and `pyproject.toml`/`poetry.lock`/`Pipfile.lock`
(no fixtures for any of them exist in this monorepo, so nothing here
would have been verified against reality); fetching npm licenses from the
registry over the network when the lockfile and `node_modules` both come
up empty (doable, but it makes an offline tool an online one, and the
`atmos/ui` case above shows exactly how much it would buy); filtering
Cargo dev-/build-/other-platform dependencies out of the report; and
license *text* verification — this trusts the declared SPDX field in
every ecosystem and never reads or diffs an actual LICENSE file, so a
package that declares `MIT` and ships something else will pass.
