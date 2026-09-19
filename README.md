# codecairn

Model-free project map for AI assistants. Reads a codebase and prints a Markdown map an AI can read first — no model involved.

## Features

- **Overview**: README summary, languages by line count, manifests (name, description, dependencies, scripts), likely entry points, test count
- **Structure**: Every directory with a guessed role (tests, routes, models, etc.), file counts and file names
- **Key files**: Files most other files depend on (Aider-style repo map), each with a purpose line from its top comment/docstring, its public symbols, and what it depends on

## How it works (no model)

- Per-language regexes extract symbols and imports (Python, TypeScript, Rust, JavaScript)
- Imports are resolved to local files to build a dependency graph
- Files are ranked by how many others use them — the same idea behind Aider's "repo map"

## Installation

### npm (no Rust toolchain required)
```bash
npx codecairn .
# or globally
npm i -g codecairn
codecairn .
```

### Cargo (requires Rust)
```bash
cargo install codecairn
```

## Usage

```bash
# Generate markdown map (default)
codecairn /path/to/project

# Generate JSON output
codecairn /path/to/project --format json

# Limit key files shown
codecairn /path/to/project --max-key-files 30

# Output to file
codecairn /path/to/project -o map.md
```

## Example Output

```
# Project Map

## Overview

> A minimal Flask application demonstrating the factory pattern.

### Languages
- **Python**: 12,450 lines
- **TypeScript**: 3,200 lines

### Manifest
- **Name**: flask
- **Description**: A simple microframework for Python
- **Dependencies**:
  - Werkzeug: >=2.0
  - Jinja2: >=3.0

### Entry Points
- `flask/app.py`
- `tests/conftest.py`

### Test Files: 42

## Structure

### flask/ (23 files) — *python*
- `flask/__init__.py`
- `flask/app.py`
- `flask/helpers.py`
...

### tests/ (42 files) — *tests*
- `tests/test_app.py`
- `tests/test_helpers.py`
...

## Key Files

*Ranked by how many other files depend on them (Aider-style repo map)*

### `flask/typing.py` (18 dependents)

**Purpose**: Type definitions and utilities for Flask's type system.

**Public Symbols**:
- `ResponseReturnValue`
- `RouteCallable`
- ...

**Depends On**:
- `flask/globals.py`
- `flask/_compat.py`
...

---

*This map is generated heuristically using regex-based symbol/import extraction.
It may miss dynamic imports, macros, and runtime-only dependencies.
Purpose lines are extracted from source comments/docstrings where available.
Use this as a starting point; read specific files for details.*
```

## Supported Languages

- Python (`.py`)
- TypeScript (`.ts`, `.tsx`)
- Rust (`.rs`)
- JavaScript (`.js`, `.jsx`, `.mjs`, `.cjs`)

## Manifest Files Detected

- `package.json` (npm)
- `Cargo.toml` (Cargo)
- `pyproject.toml` (Python)

## Limits

- Knows structure, not intent. It can say "`db.py` is used by 7 files and defines `get_db`", but not why the project makes design choices.
- The import graph is heuristic. Regexes miss dynamic imports and macros.
- Purpose lines only exist where the code has comments. Undocumented files get symbols and dependencies only.
- It is a map, not the territory. Works best as the first thing you paste in, after which the AI asks for specific files.

## Development

From the repo root:

```bash
npm run rebuild   # clean + build + smoke (the full loop)
npm run clean     # delete target/, platform binaries, tarballs, Cargo.lock
npm run build     # cargo build + copy binaries + npm pack (all installed/available targets)
npm run smoke     # install this host's package into a temp dir and run it
```

`scripts/build.mjs` orchestrates everything and is the source of truth for how a release is produced: build each available supported Rust target (`rustup target list --installed`), copy binaries into `npm/platforms/<key>/bin/`, pack every npm garden into tarballs, then install the host package in a temp dir and run it. Generated binaries and tarballs are gitignored — always rebuild before packaging.

## License

MIT