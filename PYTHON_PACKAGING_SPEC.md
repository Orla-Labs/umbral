# Python Packaging Standards: Complete Technical Reference

> For building a uv-alternative package manager. Covers every specification a compliant tool must implement.

---

## Table of Contents

1. [PEP Standards Overview](#1-pep-standards-overview)
2. [pyproject.toml Format](#2-pyprojecttoml-format)
3. [Wheel Binary Distribution Format](#3-wheel-binary-distribution-format)
4. [Source Distribution (sdist) Format](#4-source-distribution-sdist-format)
5. [PyPI Simple Repository API](#5-pypi-simple-repository-api)
6. [Version Specifiers (PEP 440)](#6-version-specifiers-pep-440)
7. [Dependency Specifiers (PEP 508)](#7-dependency-specifiers-pep-508)
8. [Environment Markers](#8-environment-markers)
9. [Build Backends](#9-build-backends)
10. [Core Metadata Format](#10-core-metadata-format)
11. [Recording Installed Packages](#11-recording-installed-packages)
12. [Package Name Normalization](#12-package-name-normalization)
13. [Platform Compatibility Tags](#13-platform-compatibility-tags)

---

## 1. PEP Standards Overview

### PEP 517 — Build Backend Interface

Defines the hook-based API that build frontends (pip, uv, your tool) use to invoke build backends.

**Mandatory Hooks:**

```python
def build_wheel(wheel_directory, config_settings=None, metadata_directory=None) -> str:
    """Build a .whl file. Returns the basename of the created file."""

def build_sdist(sdist_directory, config_settings=None) -> str:
    """Build a .tar.gz sdist. Returns the basename of the created file."""
```

**Optional Hooks:**

```python
def get_requires_for_build_wheel(config_settings=None) -> list[str]:
    """Return additional PEP 508 build deps beyond pyproject.toml [build-system].requires.
    Default: []"""

def get_requires_for_build_sdist(config_settings=None) -> list[str]:
    """Return additional deps needed for sdist creation. Default: []"""

def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None) -> str:
    """Create .dist-info directory with wheel metadata (without building the wheel).
    Returns basename of created .dist-info directory.
    Enables fast dependency resolution without full builds."""
```

**Backend Location** — specified in `pyproject.toml`:

```toml
[build-system]
requires = ["flit_core"]
build-backend = "flit_core.buildapi"        # module:object syntax
backend-path = ["backend"]                   # optional, for in-tree backends
```

Resolution: split on `:`, import the module, getattr the object. If no `:`, the module itself is the backend.

**Frontend Responsibilities:**
- Create an isolated environment with only stdlib + declared `requires`
- Install additional deps from `get_requires_for_build_*` hooks before calling build hooks
- Run hooks in subprocesses (recommended)
- Working directory = source tree root
- Build requirement graphs must be acyclic
- If `prepare_metadata_for_build_wheel` is unavailable, fall back to `build_wheel`

**config_settings:** Dict of string key-value pairs. Duplicate keys should be combined into lists. This is the escape hatch for backend-specific configuration.

---

### PEP 518 — Build System Declaration

Defines the `[build-system]` table in `pyproject.toml`:

```toml
[build-system]
requires = ["setuptools>=42", "wheel"]   # PEP 508 dependency strings — REQUIRED
build-backend = "setuptools.build_meta"  # module path — added by PEP 517
backend-path = ["backend_dir"]           # relative paths for in-tree backends
```

- If `pyproject.toml` exists without `[build-system]`, tools should assume `requires = ["setuptools", "wheel"]`
- If the table exists but `requires` is missing, that is an **error**
- The `[tool]` table is reserved for tool-specific configuration (e.g., `[tool.pytest.ini_options]`)

---

### PEP 621 — Project Metadata in pyproject.toml

Defines the `[project]` table. Every field maps to Core Metadata (PKG-INFO / METADATA).

**Required fields:**

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | Must always be static. Normalized per PEP 503 rules |
| `version` | string | PEP 440 compliant. May be `dynamic` |

**Optional fields:**

| Field | Type | Maps to Core Metadata |
|-------|------|----------------------|
| `description` | string | `Summary` |
| `readme` | string or table | `Description` + `Description-Content-Type` |
| `requires-python` | string | `Requires-Python` |
| `license` | table `{file=}` or `{text=}` | `License` |
| `license-files` | array of globs | `License-File` (multiple) |
| `authors` | array of `{name=, email=}` | `Author` / `Author-email` |
| `maintainers` | array of `{name=, email=}` | `Maintainer` / `Maintainer-email` |
| `keywords` | array of strings | `Keywords` |
| `classifiers` | array of strings | `Classifier` (multiple) |
| `urls` | table of string→string | `Project-URL` (multiple) |
| `dependencies` | array of PEP 508 strings | `Requires-Dist` (multiple) |
| `optional-dependencies` | table of name→array | `Provides-Extra` + conditional `Requires-Dist` |
| `scripts` | table of name→ref | `console_scripts` entry point group |
| `gui-scripts` | table of name→ref | `gui_scripts` entry point group |
| `entry-points` | table of tables | Other entry point groups |
| `dynamic` | array of strings | Fields the build backend will provide |

**readme as table:**
```toml
[project]
readme = {file = "README.md", content-type = "text/markdown"}
# OR
readme = {text = "Inline description", content-type = "text/plain"}
```

**Key constraint:** A field cannot appear both statically and in `dynamic`. The `name` field cannot be dynamic.

---

### PEP 660 — Editable Installs via Build Backends

Extends PEP 517 with three hooks for editable/development installs:

```python
def build_editable(wheel_directory, config_settings=None, metadata_directory=None) -> str:
    """Build an editable wheel. Returns basename. The wheel is ephemeral —
    must NOT be cached or distributed."""

def get_requires_for_build_editable(config_settings=None) -> list[str]:
    """Additional deps for editable builds. Default: []"""

def prepare_metadata_for_build_editable(metadata_directory, config_settings=None) -> str:
    """Create .dist-info for editable install without full build."""
```

**Editable wheel** — a valid PEP 427 wheel but:
- May add extra dependencies (e.g., `editables` library) beyond what `build_wheel` produces
- Must not be cached or distributed to end users
- Three implementation strategies backends use:
  1. `.pth` file pointing to source directory (like `setup.py develop`)
  2. Proxy modules via `editables` library
  3. Symlink structures

**Installer must create** `direct_url.json` with:
```json
{
  "url": "file:///path/to/project",
  "dir_info": {"editable": true}
}
```

---

### PEP 723 — Inline Script Metadata

Embeds dependency metadata directly in Python scripts:

```python
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "requests<3",
#   "rich",
# ]
# ///

import requests
from rich.pretty import pprint
```

**Parsing regex:**
```
(?m)^# /// (?P<type>[a-zA-Z0-9-]+)$\s(?P<content>(^#(| .*)$\s)+)^# ///$
```

**Content extraction:** Remove leading `# ` (first two chars if second is space) or `#` (first char only) from each line.

**Supported fields in `script` type:** `dependencies`, `requires-python`, `[tool]` table.

**Rules:**
- TYPE identifier: ASCII letters, numbers, hyphens only
- Multiple blocks of same TYPE: error
- Unclosed blocks: ignored

---

### PEP 735 — Dependency Groups

```toml
[dependency-groups]
test = ["pytest>=7", "coverage"]
docs = ["sphinx", "sphinx-rtd-theme"]
typing = ["mypy", "types-requests"]
typing-test = [
    {include-group = "typing"},
    {include-group = "test"},
]
```

**Key rules:**
- Values are lists of PEP 508 strings or `{include-group = "name"}` objects
- Includes expand inline (equivalent to copy-paste)
- Cyclic includes = error
- No deduplication after expansion
- NOT published in built distributions (sdist/wheel metadata)
- Installing a group does NOT install the package itself (unlike extras)
- Keys normalized same as package names before comparison
- Tools should validate lazily (only groups they actually use)
- Path dependencies, URL deps, editable installs are explicitly OUT OF SCOPE

---

## 2. pyproject.toml Format

### Top-Level Tables

```toml
[build-system]        # Build tool declaration (PEP 517/518)
requires = [...]      # PEP 508 dependency strings
build-backend = "..." # module:object
backend-path = [...]  # in-tree backend paths

[project]             # Project metadata (PEP 621)
name = "..."
version = "..."
# ... all fields from PEP 621

[tool.*]              # Tool-specific configuration
# e.g., [tool.ruff], [tool.mypy], [tool.hatch.build]

[dependency-groups]   # Dependency groups (PEP 735)
# e.g., test = ["pytest"]
```

**All tables are optional.** A `pyproject.toml` may exist with only `[tool]` config.

### build-backend Resolution

Given `build-backend = "setuptools.build_meta:__legacy__"`:
1. Import `setuptools.build_meta`
2. Get attribute `__legacy__`
3. Call hooks on that object

Given `build-backend = "hatchling.build"`:
1. Import `hatchling.build`
2. Call hooks directly on the module

### backend-path

```toml
[build-system]
build-backend = "my_backend"
backend-path = ["_build"]
```

- Paths are relative to project root
- Must remain within source tree
- Added to the front of `sys.path` before importing the backend

---

## 3. Wheel Binary Distribution Format

### Filename Convention

```
{distribution}-{version}(-{build})?-{python}-{abi}-{platform}.whl
```

Examples:
```
django-4.2.1-py3-none-any.whl
numpy-1.24.3-cp311-cp311-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
cryptography-41.0.0-cp37-abi3-macosx_10_12_universal2.whl
```

**Escaping:** Distribution name: replace runs of `-_.` with `_`, lowercase. Version: normalize per PEP 440.

### Internal Structure (ZIP archive, UTF-8 filenames)

```
{distribution}-{version}.dist-info/
    METADATA          # Required — Core Metadata format
    WHEEL             # Required — wheel-specific metadata
    RECORD            # Required — file manifest with hashes
    entry_points.txt  # Optional — entry points
    top_level.txt     # Optional — top-level packages
    licenses/         # Optional — license files (PEP 639)

{distribution}-{version}.data/
    scripts/          # Executable scripts
    headers/          # C header files
    data/             # Data files
    purelib/          # Pure Python modules (alternative location)
    platlib/          # Platform-specific modules (alternative location)

# Package files at root (installed to purelib or platlib based on Root-Is-Purelib)
mypackage/
    __init__.py
    module.py
```

### WHEEL File

```
Wheel-Version: 1.0
Generator: hatchling 1.18.0
Root-Is-Purelib: true
Tag: py3-none-any
```

- `Wheel-Version`: Major.Minor. Installers must reject if major > supported
- `Root-Is-Purelib`: `true` → root files go to purelib (site-packages). `false` → platlib
- `Tag`: One or more lines. Expanded compatibility tags

### RECORD File

CSV format — every file in the wheel except RECORD itself:

```
mypackage/__init__.py,sha256=AbCdEf123456...,1234
mypackage/module.py,sha256=GhIjKl789012...,5678
mypackage-1.0.dist-info/METADATA,sha256=MnOpQr...,2048
mypackage-1.0.dist-info/WHEEL,sha256=StUvWx...,110
mypackage-1.0.dist-info/RECORD,,
```

Format: `path,hash_algorithm=urlsafe_base64_digest,filesize`

- Hash must be sha256 or stronger
- RECORD's own entry has empty hash and size
- Installers MUST verify all hashes during extraction

### METADATA File

Core Metadata format (RFC 822 email headers). See [Section 10](#10-core-metadata-format).

### Installation Process

1. Parse WHEEL metadata, verify version compatibility
2. Extract root files to purelib or platlib (based on `Root-Is-Purelib`)
3. Move `.data/` subdirectories to their install scheme paths
4. Rewrite `#!python` shebangs in scripts to actual interpreter path
5. Update RECORD with installed file paths
6. Compile `.py` → `.pyc`
7. Create INSTALLER file

**Important:** `.pyc` files are NOT included in wheels. Wheels do NOT contain `setup.py` or `setup.cfg`.

---

## 4. Source Distribution (sdist) Format

### Filename

```
{name}-{version}.tar.gz
```

Name normalized (same rules as wheel distribution name), version in canonical form.

### Structure

```
{name}-{version}/
    pyproject.toml     # Required
    PKG-INFO           # Required (Core Metadata ≥ 2.2)
    src/
    tests/
    ... (all source files)
```

### Requirements

- Single top-level directory named `{name}-{version}`
- **POSIX.1-2001 pax tar format** with gzip compression
- UTF-8 filenames
- Must be readable via `tarfile.open(path, 'r:gz')`
- If metadata ≥ 2.4 and `License-File` fields exist, those files must be included

### Security

Extractors must:
- Reject files placed outside destination directory
- Reject links pointing externally
- Reject device files
- Reject entries with `..` components
- Strip leading slashes
- Clear setuid/setgid/sticky bits
- Use `tarfile.data_filter()` when available

---

## 5. PyPI Simple Repository API

### PEP 503 — HTML Simple API

**Root endpoint** `GET /simple/`:
```html
<!DOCTYPE html>
<html>
<body>
  <a href="/simple/requests/">requests</a>
  <a href="/simple/flask/">flask</a>
  ...
</body>
</html>
```

**Project endpoint** `GET /simple/{normalized-name}/`:
```html
<!DOCTYPE html>
<html>
<body>
  <a href="https://files.pythonhosted.org/.../requests-2.31.0.tar.gz#sha256=abcdef..."
     data-requires-python="&gt;=3.7"
     data-dist-info-metadata="sha256=123abc..."
     >requests-2.31.0.tar.gz</a>
  <a href="https://files.pythonhosted.org/.../requests-2.31.0-py3-none-any.whl#sha256=fedcba..."
     data-requires-python="&gt;=3.7"
     data-dist-info-metadata="sha256=456def..."
     data-yanked="critical security issue"
     >requests-2.31.0-py3-none-any.whl</a>
</body>
</html>
```

**Project name normalization for URLs:**
```python
re.sub(r"[-_.]+", "-", name).lower()
```

**Anchor attributes:**
| Attribute | Value | PEP |
|-----------|-------|-----|
| `href` | URL with `#hashname=hashvalue` fragment | 503 |
| `data-requires-python` | PEP 440 version specifier (HTML-escaped) | 503 |
| `data-dist-info-metadata` | `true` or `hashname=hashvalue` | 658 |
| `data-yanked` | Empty or reason string | 592 |
| `data-gpg-sig` | `true` or `false` | 503 |

### PEP 691 — JSON Simple API

**Content negotiation via Accept header:**
```
Accept: application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.2, text/html;q=0.01
```

**Content types:**
- `application/vnd.pypi.simple.v1+json` — JSON format
- `application/vnd.pypi.simple.v1+html` — HTML format
- `text/html` — Legacy HTML alias

**Root endpoint JSON:**
```json
{
  "meta": {"api-version": "1.0"},
  "projects": [
    {"name": "requests"},
    {"name": "flask"}
  ]
}
```

**Project endpoint JSON:**
```json
{
  "meta": {"api-version": "1.0"},
  "name": "requests",
  "files": [
    {
      "filename": "requests-2.31.0-py3-none-any.whl",
      "url": "https://files.pythonhosted.org/.../requests-2.31.0-py3-none-any.whl",
      "hashes": {
        "sha256": "abcdef1234567890..."
      },
      "requires-python": ">=3.7",
      "dist-info-metadata": {"sha256": "123abc..."},
      "yanked": false
    }
  ]
}
```

**File object fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `filename` | string | Yes | Distribution filename |
| `url` | string | Yes | Download URL |
| `hashes` | `{algo: hex_digest}` | Yes | At least one hash |
| `requires-python` | string | No | PEP 440 version specifier |
| `dist-info-metadata` | bool or `{algo: hex_digest}` | No | PEP 658 metadata availability |
| `yanked` | bool or string | No | PEP 592 yank status/reason |
| `gpg-sig` | bool | No | GPG signature availability |

### PEP 658 — Metadata Without Full Download

When `data-dist-info-metadata` is present, the METADATA file is available at:
```
{file_url}.metadata
```

Example: if wheel URL is `.../requests-2.31.0-py3-none-any.whl`, metadata is at `.../requests-2.31.0-py3-none-any.whl.metadata`.

This enables fast dependency resolution: fetch 5KB metadata instead of 500KB+ wheel.

### PEP 592 — Yanked Releases

- Installers MUST ignore yanked releases if a non-yanked version satisfies constraints
- Exception: exact pins via `==` or `===` may use yanked releases
- Installers SHOULD warn when installing a yanked release
- Mirrors must preserve the `data-yanked` attribute if they mirror the file

---

## 6. Version Specifiers (PEP 440)

### Version Format

```
[N!]N(.N)*[{a|b|rc}N][.postN][.devN][+local]
```

| Segment | Format | Examples | Required |
|---------|--------|----------|----------|
| Epoch | `N!` | `1!`, `2!` | No (default 0) |
| Release | `N(.N)*` | `1.0`, `2.3.1`, `2024.1` | Yes |
| Pre-release | `{a\|b\|rc}N` | `a0`, `b1`, `rc2` | No |
| Post-release | `.postN` | `.post0`, `.post1` | No |
| Dev release | `.devN` | `.dev0`, `.dev5` | No |
| Local | `+label` | `+ubuntu.1`, `+build.42` | No |

### Canonical Version Regex

```python
import re

def is_canonical(version: str) -> bool:
    return re.match(
        r'^([1-9][0-9]*!)?(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))*'
        r'((a|b|rc)(0|[1-9][0-9]*))?'
        r'(\.post(0|[1-9][0-9]*))?'
        r'(\.dev(0|[1-9][0-9]*))?$',
        version
    ) is not None
```

### Version Normalization Rules

| Input | Normalized | Rule |
|-------|-----------|------|
| `1.1RC1` | `1.1rc1` | Lowercase |
| `01.02.03` | `1.2.3` | Strip leading zeros |
| `1.1.a1` | `1.1a1` | Remove pre-release separator |
| `1.1-alpha1` | `1.1a1` | `alpha` → `a` |
| `1.1beta2` | `1.1b2` | `beta` → `b` |
| `1.1c3` | `1.1rc3` | `c` → `rc` |
| `1.1-preview2` | `1.1rc2` | `preview` → `rc` |
| `1.2-post2` | `1.2.post2` | Normalize post separator |
| `1.2.rev3` | `1.2.post3` | `rev`/`r` → `post` |
| `1.0-1` | `1.0.post1` | Implicit post-release |
| `1.2-dev2` | `1.2.dev2` | Normalize dev separator |
| `1.2.dev` | `1.2.dev0` | Implicit dev number |
| `1.2a` | `1.2a0` | Implicit pre number |
| `1.2.post` | `1.2.post0` | Implicit post number |
| `v1.0` | `1.0` | Strip `v` prefix |
| `1.0+ubuntu-1` | `1.0+ubuntu.1` | Normalize local separators |

### Version Ordering

**Complete sort order:**
```
.devN < aN < bN < rcN < (final) < .postN
```

**Within a suffix type:** ordered by numeric value (`a1 < a2 < a12`)

**Compound suffixes:**
```
1.0.dev0
1.0a1.dev0
1.0a1
1.0a1.post0.dev0
1.0a1.post0
1.0b1
1.0rc1
1.0
1.0.post0.dev0
1.0.post0
1.0.post1
1.1.dev0
```

**Epoch:** Higher epoch always wins: `1!0.1 > 999.999`

**Release segment:** Compare as integer tuples, shorter segments padded with zeros: `1.0 == 1.0.0`

**Local versions:** Only meaningful for same public version. Numeric segments > alpha segments. More segments > fewer matching segments.

### Comparison Operators

| Operator | Meaning | Example | Matches |
|----------|---------|---------|---------|
| `~=` | Compatible release | `~=1.4.5` | `>=1.4.5, ==1.4.*` |
| `==` | Exact match | `==1.0` | Only `1.0` (or `1.0.0`) |
| `==X.*` | Prefix match | `==1.1.*` | `1.1`, `1.1.0`, `1.1.1`, `1.1a1`, etc. |
| `!=` | Exclusion | `!=1.5` | Everything except `1.5` |
| `!=X.*` | Prefix exclusion | `!=1.1.*` | Everything outside `1.1.*` |
| `<=` | Inclusive upper | `<=2.0` | Up to and including `2.0` |
| `>=` | Inclusive lower | `>=1.0` | `1.0` and above |
| `<` | Exclusive upper | `<2.0` | Below `2.0` (NOT `2.0a1`) |
| `>` | Exclusive lower | `>1.0` | Above `1.0` (NOT `1.0.post1`) |
| `===` | Arbitrary equality | `===foobar` | Literal string match |

**Compatible release (`~=`) expansion:**
- `~=2.2` → `>=2.2, ==2.*`
- `~=1.4.5` → `>=1.4.5, ==1.4.*`
- `~=2.2.0` → `>=2.2.0, ==2.2.*`
- Cannot be used with single-segment versions (`~=1` is invalid)

**Exclusive comparison edge cases:**
- `>1.7` allows `1.7.1` but NOT `1.7.0.post1`
- `>1.7.post2` allows `1.7.1` and `1.7.0.post3`
- `<2.0` allows `1.9.6` but NOT `2.0a1`
- `<2.0rc1` allows `2.0a1` and `2.0b1`

**Pre-release handling defaults:**
- Excluded unless: already installed, explicitly requested, or only available version
- Post-releases and final releases: always included

**Local version handling in `==`:**
- Specifier without local: candidate local labels ignored (`==1.0` matches `1.0+local`)
- Specifier with local: strict match required

---

## 7. Dependency Specifiers (PEP 508)

### Grammar

```
specification = wsp* ( url_req | name_req ) wsp*
name_req      = name wsp* extras? wsp* versionspec? wsp* quoted_marker?
url_req       = name wsp* extras? wsp* '@' wsp* URI (wsp+ | end) quoted_marker?
```

### Name-Based Requirements

```
requests
requests>=2.28
requests[security]>=2.28,<3.0
requests[security,tests]>=2.28; python_version >= "3.8"
```

**Name pattern:** `^([A-Z0-9]|[A-Z0-9][A-Z0-9._-]*[A-Z0-9])$` (case-insensitive)

**Extras:** `[extra1, extra2]` — union of their dependencies plus base package

**Version specifiers:** Comma-separated, combined with AND:
```
>= 2.8.1, == 2.8.*, != 2.8.3
```

Parentheses optional but allowed: `(>=2.0, <3.0)`

### URL-Based Requirements

```
pip @ https://github.com/pypa/pip/archive/1.3.1.zip#sha1=da9234ee...
mypackage @ file:///local/path/to/package.whl
mypackage @ git+https://github.com/user/repo.git@main
```

### Complete EBNF Grammar

```ebnf
wsp           = ' ' | '\t'
letter        = 'A'-'Z' | 'a'-'z'
digit         = '0'-'9'
letterOrDigit = letter | digit

identifier_end = letterOrDigit | (('-' | '_' | '.') * letterOrDigit)
identifier     = letterOrDigit identifier_end*
name           = identifier

extras_list = identifier (',' wsp* identifier)*
extras      = '[' wsp* extras_list? wsp* ']'

version_cmp  = wsp* ('<=' | '<' | '!=' | '==' | '>=' | '>' | '~=' | '===')
version      = wsp* (letterOrDigit | '-' | '_' | '.' | '*' | '+' | '!')+
version_one  = version_cmp version wsp*
version_many = version_one (',' version_one)*
versionspec  = ('(' version_many ')') | version_many

urlspec = '@' wsp* URI_reference

marker_op    = version_cmp | (wsp+ 'in') | (wsp+ 'not' wsp+ 'in')
python_str_c = wsp | letter | digit | '(' | ')' | '.' | '{' | '}' |
               '-' | '_' | '*' | '#' | ':' | ';' | ',' | '/' | '?' |
               '[' | ']' | '!' | '~' | '`' | '@' | '$' | '%' | '^' |
               '&' | '=' | '+' | '|' | '<' | '>'
python_str   = ("'" (python_str_c | '"')* "'") |
               ('"' (python_str_c | "'")* '"')
env_var      = 'python_version' | 'python_full_version' | 'os_name' |
               'sys_platform' | 'platform_release' | 'platform_system' |
               'platform_version' | 'platform_machine' |
               'platform_python_implementation' | 'implementation_name' |
               'implementation_version' | 'extra'
marker_var   = wsp* (env_var | python_str)
marker_expr  = marker_var marker_op marker_var | wsp* '(' marker wsp* ')'
marker_and   = marker_expr wsp* 'and' marker_expr | marker_expr
marker_or    = marker_and wsp* 'or' marker_and | marker_and
marker       = marker_or
quoted_marker = ';' wsp* marker

name_req = name wsp* extras? wsp* versionspec? wsp* quoted_marker?
url_req  = name wsp* extras? wsp* urlspec (wsp+ | end) quoted_marker?
specification = wsp* (url_req | name_req) wsp*
```

---

## 8. Environment Markers

### Available Variables

| Variable | Source | Example Values |
|----------|--------|---------------|
| `python_version` | `'.'.join(platform.python_version_tuple()[:2])` | `"3.11"`, `"3.12"` |
| `python_full_version` | `platform.python_version()` | `"3.11.4"`, `"3.12.0b1"` |
| `os_name` | `os.name` | `"posix"`, `"nt"`, `"java"` |
| `sys_platform` | `sys.platform` | `"linux"`, `"darwin"`, `"win32"` |
| `platform_release` | `platform.release()` | `"6.1.0"`, `"22.5.0"` |
| `platform_system` | `platform.system()` | `"Linux"`, `"Darwin"`, `"Windows"` |
| `platform_version` | `platform.version()` | OS-specific version string |
| `platform_machine` | `platform.machine()` | `"x86_64"`, `"aarch64"`, `"arm64"` |
| `platform_python_implementation` | `platform.python_implementation()` | `"CPython"`, `"PyPy"` |
| `implementation_name` | `sys.implementation.name` | `"cpython"`, `"pypy"` |
| `implementation_version` | from `sys.implementation.version` | `"3.11.4"` |
| `extra` | Context-dependent | `"test"`, `"dev"` (only in wheel METADATA) |

### Marker Operators

- **Version comparison:** `<`, `<=`, `!=`, `==`, `>=`, `>`, `~=`, `===`
- **String operations:** `in`, `not in`
- **Logical:** `and`, `or` (`and` binds tighter)

### Evaluation Rules

1. When both operands are valid PEP 440 versions → use version comparison
2. Otherwise → fall back to string comparison
3. `~=` with non-version operands → error
4. Unknown variables → error (not silent true/false)
5. Missing/uncalculable variables → `"0"` for versions, `""` for strings
6. Result: `True` (include dependency) or `False` (ignore dependency)

### Common Patterns

```python
# Platform-specific deps
'pywin32; sys_platform == "win32"'
'uvloop; sys_platform != "win32"'

# Python version gates
'importlib-metadata; python_version < "3.8"'
'tomli; python_version < "3.11"'

# Architecture-specific
'tensorflow-aarch64; platform_machine == "aarch64"'

# Implementation-specific
'cffi; implementation_name == "cpython"'

# Combined conditions
'foo; python_version >= "3.8" and sys_platform == "linux"'
'bar; (python_version >= "3.8" and sys_platform == "linux") or sys_platform == "darwin"'
```

---

## 9. Build Backends

### Common Build Backends

| Backend | build-backend | requires | Use Case |
|---------|--------------|----------|----------|
| setuptools | `setuptools.build_meta` | `["setuptools>=42"]` | Legacy + compiled extensions |
| hatchling | `hatchling.build` | `["hatchling"]` | Modern pure Python |
| flit-core | `flit_core.buildapi` | `["flit_core>=3.11,<4"]` | Simple pure Python |
| pdm-backend | `pdm.backend` | `["pdm-backend"]` | Modern, PEP 621 native |
| maturin | `maturin` | `["maturin>=1.0,<2.0"]` | Rust+Python extensions |

### How Build Backends Work

A build frontend (your package manager) must:

1. **Read `pyproject.toml`** → extract `[build-system]`
2. **Create isolated environment** with only `requires` installed
3. **Import the backend** module (optionally via `backend-path`)
4. **Call `get_requires_for_build_wheel()`** → install returned deps
5. **Call `prepare_metadata_for_build_wheel()`** → get metadata for resolution (fast path)
6. **Call `build_wheel()` or `build_sdist()`** → produce artifact

**For editable installs**, use the PEP 660 hooks instead (`build_editable`, etc.).

### setuptools

```toml
[build-system]
requires = ["setuptools>=42", "wheel"]
build-backend = "setuptools.build_meta"
```

- Supports `setup.py`, `setup.cfg`, and `pyproject.toml` configuration
- `setup_requires` in setup.cfg/setup.py is deprecated — use `[build-system].requires`
- In-tree backend wrappers via `backend-path` for dynamic build deps
- Implements all PEP 517 + PEP 660 hooks

### hatchling

```toml
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
```

- Configuration under `[tool.hatch.build]` and `[tool.hatch.build.targets.*]`
- Built-in targets: `wheel`, `sdist`, `custom`
- File selection: `include`, `exclude`, `artifacts`, `only-include`, `packages`, `force-include`, `sources`
- VCS-aware: respects `.gitignore`/`.hgignore` by default
- Plugin system for builders and hooks
- Standards-compliant PEP 517 + PEP 660

### flit-core

```toml
[build-system]
requires = ["flit_core>=3.11,<4"]
build-backend = "flit_core.buildapi"
```

- Minimal, no `setup.py` needed
- Extracts `__version__` and docstring from main module
- PEP 621 metadata (`[project]` table)
- Dynamic fields: `version` and `description` from module
- Pure Python only (no compiled extensions)

### pdm-backend

```toml
[build-system]
requires = ["pdm-backend"]
build-backend = "pdm.backend"
```

- PEP 517 + PEP 621 + PEP 660 compliant
- Successor to `pdm-pep517`

### maturin

```toml
[build-system]
requires = ["maturin>=1.0,<2.0"]
build-backend = "maturin"
```

- Builds Rust extensions for Python via PyO3/cffi/uniffi
- Configuration in `[tool.maturin]`
- Handles cross-compilation, platform-specific wheels, manylinux compliance
- Binding types: `pyo3` (default), `cffi`, `bin`, `uniffi`

---

## 10. Core Metadata Format

Used in `PKG-INFO` (sdist) and `METADATA` (wheel). RFC 822 email header format, parsed by Python's `email.parser` with `compat32` policy. UTF-8 encoded.

### Required Fields

```
Metadata-Version: 2.4
Name: my-package
Version: 1.0.0
```

### All Fields

```
Metadata-Version: 2.4
Name: my-package
Version: 1.0.0
Dynamic: classifiers
Dynamic: description
Summary: A short one-line summary
Description-Content-Type: text/markdown
Keywords: packaging,tools
Home-page: https://example.com
Author: Jane Doe
Author-email: Jane Doe <jane@example.com>
Maintainer: John Smith
Maintainer-email: John Smith <john@example.com>
License: MIT
License-Expression: MIT AND Apache-2.0
License-File: LICENSE
License-File: NOTICE
Classifier: Development Status :: 3 - Alpha
Classifier: Programming Language :: Python :: 3
Requires-Dist: requests>=2.28
Requires-Dist: click>=8.0
Requires-Dist: uvloop; sys_platform != "win32"
Requires-Dist: colorama; extra == "color"
Requires-Python: >=3.8
Requires-External: libffi
Project-URL: Homepage, https://example.com
Project-URL: Repository, https://github.com/user/repo
Provides-Extra: color
Provides-Extra: dev
Platform: any

[blank line]
Full description body goes here (alternative to Description header).
Multi-line content in markdown or rst.
```

**Multi-valued fields** (appear multiple times): `Dynamic`, `Platform`, `Supported-Platform`, `Classifier`, `Requires-Dist`, `Provides-Extra`, `Project-URL`, `License-File`, `Provides-Dist`, `Obsoletes-Dist`, `Requires-External`

**Description body:** After a blank line, the remaining content is the long description. Alternative to `Description` header with CRLF+7-space+pipe folding.

### Metadata Versions

| Version | Key Additions |
|---------|--------------|
| 1.0 | Name, Version, Summary |
| 1.1 | Classifier, Requires, Provides, Obsoletes |
| 1.2 | Requires-Dist, Requires-Python, Requires-External, Project-URL |
| 2.1 | Description-Content-Type, Provides-Extra |
| 2.2 | (formalization) |
| 2.3 | (further formalization) |
| 2.4 | License-Expression, License-File |
| 2.5 | Import-Name, Import-Namespace |

---

## 11. Recording Installed Packages

When installing a package, the installer must create a `.dist-info` directory in site-packages.

### Directory Name

```
{normalized_name}-{normalized_version}.dist-info/
```

Both name and version normalized, dashes replaced with underscores.

### Required Contents

**METADATA** — Core metadata file (mandatory, must succeed or installation fails)

### Optional Contents

| File | Purpose |
|------|---------|
| `RECORD` | CSV of all installed files with hashes |
| `INSTALLER` | Single line: name of installing tool |
| `entry_points.txt` | Entry points in INI format |
| `direct_url.json` | Required for direct URL/VCS/editable installs |
| `licenses/` | License files (metadata ≥ 2.4) |

### RECORD Format

```csv
../mypackage/__init__.py,sha256=base64digest,1234
../mypackage/module.py,sha256=base64digest,5678
mypackage-1.0.dist-info/METADATA,sha256=base64digest,2048
mypackage-1.0.dist-info/RECORD,,
```

- CSV with default Python `csv.reader` dialect
- Paths: absolute or relative to `.dist-info` parent directory
- Hash: `algorithm=urlsafe_base64_nopadding` (from `hashlib.algorithms_guaranteed`)
- RECORD's own entry: empty hash and size

### direct_url.json

Required when installing from a direct URL, VCS reference, or local directory.

**Archive install:**
```json
{
  "url": "https://example.com/package-1.0.tar.gz",
  "archive_info": {
    "hashes": {"sha256": "abcdef..."}
  }
}
```

**VCS install:**
```json
{
  "url": "https://github.com/user/repo.git",
  "vcs_info": {
    "vcs": "git",
    "commit_id": "abc123def456...",
    "requested_revision": "main"
  }
}
```

**Editable install:**
```json
{
  "url": "file:///home/user/projects/mypackage",
  "dir_info": {"editable": true}
}
```

**Local directory (non-editable):**
```json
{
  "url": "file:///home/user/projects/mypackage",
  "dir_info": {}
}
```

### entry_points.txt Format

```ini
[console_scripts]
mycli = mypackage.cli:main

[gui_scripts]
mygui = mypackage.gui:launch

[mypackage.plugins]
plugin1 = mypackage.plugins.one:PluginOne
plugin2 = mypackage.plugins.two:PluginTwo [extra1]
```

- INI format, case-sensitive
- `=` delimiter (not `:`)
- Object reference: `module.path:object.attr`
- Optional extras in `[brackets]` (discouraged for new tools)
- `console_scripts` → command-line executables
- `gui_scripts` → GUI executables (no console on Windows)

---

## 12. Package Name Normalization

**Algorithm:**
```python
import re
def normalize(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()
```

**Valid name pattern:** `^([A-Z0-9]|[A-Z0-9][A-Z0-9._-]*[A-Z0-9])$` (case-insensitive)

**All of these normalize to `friendly-bard`:**
- `Friendly-Bard`
- `FRIENDLY-BARD`
- `friendly.bard`
- `friendly_bard`
- `friendly--bard`
- `FrIeNdLy-._.-bArD`

**Usage:** Normalize before all comparisons, index lookups, and URL construction.

**Note for wheel filenames and `.dist-info` directories:** Replace `-` with `_` after normalization (e.g., `friendly_bard-1.0.dist-info`).

---

## 13. Platform Compatibility Tags

### Tag Format

```
{python}-{abi}-{platform}
```

### Python Tags

| Code | Implementation |
|------|---------------|
| `py` | Generic Python |
| `cp` | CPython |
| `pp` | PyPy |
| `ip` | IronPython |
| `jy` | Jython |

Version uses `py_version_nodot`: `cp311`, `py3`, `pp39`

Major-only tags (`py3`) mean cross-version compatibility, not shorthand for `py30`.

### ABI Tags

| Tag | Meaning |
|-----|---------|
| `cp311` | CPython 3.11 specific ABI |
| `cp311d` | CPython 3.11 debug build |
| `abi3` | CPython stable ABI (forward-compatible) |
| `none` | No ABI requirement (pure Python) |

### Platform Tags

**Linux (glibc):**
```
manylinux_2_17_x86_64    # glibc ≥ 2.17 on x86_64
manylinux_2_28_aarch64   # glibc ≥ 2.28 on aarch64
manylinux2014_x86_64     # legacy alias for glibc ≥ 2.17
manylinux2010_x86_64     # legacy alias for glibc ≥ 2.12
manylinux1_x86_64        # legacy alias for glibc ≥ 2.5
linux_x86_64             # specific Linux (not portable)
```

**Linux (musl):**
```
musllinux_1_1_x86_64     # musl ≥ 1.1 on x86_64
musllinux_1_2_aarch64    # musl ≥ 1.2 on aarch64
```

**macOS:**
```
macosx_10_12_x86_64      # macOS ≥ 10.12 on Intel
macosx_11_0_arm64        # macOS ≥ 11.0 on Apple Silicon
macosx_10_12_universal2  # macOS ≥ 10.12, arm64 + x86_64 fat binary
```

**Windows:**
```
win32                    # 32-bit Windows
win_amd64                # 64-bit Windows
win_arm64                # Windows on ARM
```

**Pure Python:**
```
any                      # Platform-independent
```

**Mobile:**
```
android_27_arm64_v8a     # Android API 27+ on arm64
ios_16_0_arm64_iphoneos  # iOS 16.0+ on device
```

### Tag Priority (most to least preferred for CPython 3.11 on linux_x86_64)

```
1.  cp311-cp311-linux_x86_64
2.  cp311-cp311-manylinux_2_35_x86_64
3.  ...down through manylinux versions...
4.  cp311-abi3-linux_x86_64
5.  cp311-abi3-manylinux_2_35_x86_64
6.  ...
7.  cp311-none-linux_x86_64
8.  cp311-none-manylinux_2_35_x86_64
9.  ...
10. cp311-none-any
11. cp3-none-any
12. py311-none-any
13. py3-none-any
14. py310-none-any   (and older)
```

### Compressed Tag Sets

Wheel filenames use dot-separated tags for multi-compatibility:
```
py2.py3-none-any.whl
cp311.cp312-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
```

Expansion:
```python
for x in pytag.split('.'):
    for y in abitag.split('.'):
        for z in platformtag.split('.'):
            yield f"{x}-{y}-{z}"
```

### manylinux Compatibility

Older glibc tags are forward-compatible:
- A `manylinux_2_17` wheel runs on glibc ≥ 2.17
- A system with glibc 2.35 supports: `manylinux_2_35`, `manylinux_2_34`, ..., `manylinux_2_17`

### macOS Version Compatibility

- macOS 10.x: `macosx_10_y_arch`
- macOS 11+: `macosx_x_0_arch` (only major version matters)
- A system on macOS 13.0 supports: `macosx_13_0`, `macosx_12_0`, `macosx_11_0`, `macosx_10_16`, ..., `macosx_10_4`

### Architecture Multi-Arch Tags (macOS)

| Tag | Architectures |
|-----|--------------|
| `universal2` | arm64 + x86_64 |
| `universal` | i386 + ppc + ppc64 + x86_64 |
| `intel` | i386 + x86_64 |

---

## Implementation Checklist for a Package Manager

### Core Parsers Needed
- [ ] PEP 440 version parser + comparator + normalizer
- [ ] PEP 508 dependency specifier parser (name, extras, version, URL, markers)
- [ ] Environment marker evaluator
- [ ] pyproject.toml reader (TOML parser + PEP 621 field extraction)
- [ ] Package name normalizer
- [ ] Platform compatibility tag generator + matcher

### Index Client
- [ ] PEP 503 HTML Simple API parser
- [ ] PEP 691 JSON Simple API client
- [ ] Content negotiation (Accept headers)
- [ ] PEP 658 metadata fetching (`.metadata` files)
- [ ] PEP 592 yanked release handling
- [ ] Hash verification on downloads

### Build System Integration
- [ ] PEP 517 build frontend (invoke backend hooks in isolated environments)
- [ ] PEP 518 build dependency installation
- [ ] PEP 660 editable install support
- [ ] Backend discovery (module:object resolution, backend-path)
- [ ] Build isolation (clean environment with only declared deps)

### Distribution Handling
- [ ] Wheel reader: ZIP extraction, METADATA/WHEEL/RECORD parsing, tag matching
- [ ] Wheel installer: file placement, shebang rewriting, RECORD update, .pyc compilation
- [ ] Sdist reader: tar.gz extraction, security filtering
- [ ] Sdist builder invocation (via PEP 517 `build_sdist`)

### Installation Recording
- [ ] Create `.dist-info` directory with METADATA, RECORD, INSTALLER
- [ ] Generate `direct_url.json` for URL/VCS/editable installs
- [ ] Write `entry_points.txt`
- [ ] Generate console/GUI script wrappers

### Metadata & Configuration
- [ ] Core Metadata reader/writer (RFC 822 format)
- [ ] PEP 723 inline script metadata parser
- [ ] PEP 735 dependency group resolver
- [ ] entry_points.txt reader/writer

### Dependency Resolution
- [ ] SAT solver or backtracking resolver
- [ ] Handle extras (union of conditional deps)
- [ ] Handle environment markers (filter deps by target platform)
- [ ] Handle pre-release filtering logic
- [ ] Handle yanked release logic
- [ ] Lock file generation and reading
