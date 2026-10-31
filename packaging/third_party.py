#!/usr/bin/env python3
"""Collect the license and notice files of every crate built into Reeve.

    packaging/third_party.py <out-dir>

Writes <out-dir>/README.md, an index of the locked third-party crates with
each one's declared license and a link to its published source, and next to
it one file per notice, named <crate>-<version>-<FILE> (a notice inside a
subdirectory has its path joined with "-"), so the directory is flat and a
package's file list can take it whole. The release tarball, the RPM and the
.deb ship it as third-party/. Run from the repository root; packaging/dist.sh
runs it for you.
"""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

NOTICE = ("license", "licence", "copying", "copyright", "notice", "unlicense", "acknowledg")


def main(out: Path) -> None:
    cargo = os.environ.get("CARGO", "cargo")
    meta = json.loads(
        subprocess.check_output([cargo, "metadata", "--locked", "--format-version", "1"], text=True)
    )
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    lines = [
        "# Third-party notices",
        "",
        "The crates built into this release of Reeve, with their declared licenses. Each notice",
        "file here is copied unchanged from the crate; the link is its source as published.",
        "",
    ]
    crates = sorted((p for p in meta["packages"] if p["source"]), key=lambda p: (p["name"], p["version"]))
    for pkg in crates:
        name, version = pkg["name"], pkg["version"]
        root = Path(pkg["manifest_path"]).parent
        copied = []
        for entry in sorted(root.iterdir()):
            if entry.is_symlink() or not entry.name.lower().startswith(NOTICE):
                continue
            files = [entry] if entry.is_file() else sorted(
                f for f in entry.rglob("*") if f.is_file() and not f.is_symlink()
            )
            for f in files:
                flat = f"{name}-{version}-" + "-".join(f.relative_to(root).parts)
                shutil.copyfile(f, out / flat)
                copied.append(flat)
        lines += [
            f"## {name} {version}",
            "",
            f"License: `{pkg.get('license') or 'see the source'}`. "
            f"[Source](https://crates.io/crates/{name}/{version}).",
            ("Notices: " + ", ".join(f"[{c}]({c})" for c in copied))
            if copied
            else "Notices: none in the crate; see its source.",
            "",
        ]
    (out / "README.md").write_text("\n".join(lines) + "\n")
    print(f"third-party: {len(crates)} crates, {sum(1 for _ in out.iterdir()) - 1} notice files in {out}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(Path(sys.argv[1]))
