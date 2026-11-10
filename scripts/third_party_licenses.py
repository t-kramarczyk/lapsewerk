#!/usr/bin/env python3
"""Writes THIRD-PARTY-LICENSES.txt: every crate the two programs are built from, with its licence
and the licence texts it ships (font licences included), each distinct text once.

Run from the repository root after a build (`cargo metadata` needs the sources fetched):
    python3 scripts/third_party_licenses.py
"""
import hashlib
import json
import pathlib
import subprocess
import sys

NAMES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "OFL", "UFL")


MIT = """MIT License

Copyright (c) the authors of the crate (see its repository)

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE."""

ZLIB = """zlib License

Copyright (c) the authors of the crate (see its repository)

This software is provided 'as-is', without any express or implied warranty. In no event will the
authors be held liable for any damages arising from the use of this software.

Permission is granted to anyone to use this software for any purpose, including commercial
applications, and to alter it and redistribute it freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not claim that you wrote the
   original software. If you use this software in a product, an acknowledgment in the product
   documentation would be appreciated but is not required.
2. Altered source versions must be plainly marked as such, and must not be misrepresented as being
   the original software.
3. This notice may not be removed or altered from any source distribution."""

BSD3 = """BSD 3-Clause License

Copyright (c) the authors of the crate (see its repository)

Redistribution and use in source and binary forms, with or without modification, are permitted
provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this list of conditions
   and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this list of
   conditions and the following disclaimer in the documentation and/or other materials provided
   with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may be used to
   endorse or promote products derived from this software without specific prior written
   permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF
THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE."""


def repo_root(root: pathlib.Path):
    """A crate fetched from git lives inside its repository: the repository's root (where its
    LICENSE and NOTICE files are), or None for a crate from crates.io."""
    parts = root.parts
    if "checkouts" in parts:
        i = parts.index("checkouts")
        if len(parts) > i + 2:
            return pathlib.Path(*parts[: i + 3])
    return None


def licence_files(root: pathlib.Path):
    found = []
    repo = repo_root(root)
    if repo is not None and repo != root:
        found += licence_files_in(repo)
    return found + licence_files_in(root)


def licence_files_in(root: pathlib.Path):
    found = []
    for p in sorted(root.iterdir()):
        if p.is_file() and p.name.upper().startswith(NAMES):
            found.append(p)
    # Fonts carry their own licences (egui's, LightCraft's).
    for sub in ("fonts", "assets/fonts"):
        d = root / sub
        if d.is_dir():
            found += [p for p in sorted(d.rglob("*")) if p.is_file() and p.suffix.lower() in (".txt", ".md")
                      and any(k in p.name.upper() for k in ("LICEN", "OFL", "UFL", "COPYING"))]
    return found


def main():
    used = set()
    # The platforms Lapsewerk is built for: Linux and macOS (both chips).
    for platform in ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin"):
        meta = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", platform], text=True))
        ours = set(meta["workspace_members"])
        nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
        stack = list(ours)
        seen = set()
        while stack:
            i = stack.pop()
            if i in seen:
                continue
            seen.add(i)
            for d in nodes[i]["deps"]:
                # Build and normal dependencies ship; dev-only ones do not.
                if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
                    stack.append(d["pkg"])
        used |= seen
    texts = {}
    rows = []
    for p in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
        if p["id"] not in used or p["id"] in ours:
            continue
        root = pathlib.Path(p["manifest_path"]).parent
        keys = []
        for f in licence_files(root):
            text = f.read_text(errors="replace").strip()
            k = hashlib.sha256(text.encode()).hexdigest()[:12]
            texts.setdefault(k, (text, set()))[1].add(f"{p['name']} {p['version']}")
            keys.append(f"{f.name} [{k}]")
        rows.append(f"{p['name']} {p['version']}  {p.get('license') or p.get('license_file') or 'see texts'}"
                    f"  {p.get('repository') or ''}\n    texts: {', '.join(keys) or 'none shipped: the licence named applies (standard texts at the end; Apache-2.0: the LICENSE-APACHE text above)'}")
    # The standard texts, for the crates that ship none of their own (the licence they name applies
    # with their authors as the copyright holders; see each crate's repository).
    standard = {
        "MIT": MIT,
        "Zlib": ZLIB,
        "BSD-3-Clause": BSD3,
    }
    for name, text in standard.items():
        k = hashlib.sha256(text.encode()).hexdigest()[:12]
        texts.setdefault(k, (text, set()))[1].add(f"standard {name} text, for the crates naming {name} that ship no text")
    out = ["Lapsewerk: third-party components and their licences",
           "Generated by scripts/third_party_licenses.py from Cargo.lock; do not edit by hand.", "",
           f"{len(rows)} crates", ""] + rows + ["", "=" * 78, "Licence texts (each once; the crates that ship it)", "=" * 78]
    for k, (text, crates) in sorted(texts.items(), key=lambda kv: sorted(kv[1][1])[0]):
        out += ["", "-" * 78, f"[{k}] shipped by: {', '.join(sorted(crates))}", "-" * 78, text]
    pathlib.Path("THIRD-PARTY-LICENSES.txt").write_text("\n".join(out) + "\n")
    print(f"THIRD-PARTY-LICENSES.txt: {len(rows)} crates, {len(texts)} distinct texts", file=sys.stderr)


if __name__ == "__main__":
    main()
