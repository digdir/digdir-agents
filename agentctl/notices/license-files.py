#!/usr/bin/env python3
"""Print the license files that the dependencies of an agentctl release ship.

Reads a `cargo about generate --format json` report and, for every crate from a
registry or Git source, reproduces the license, copying, copyright and notice
files in its package root verbatim. cargo-about identifies license texts
automatically and falls back to canonical texts without the crate's copyright
notices when it cannot, so this keeps every notice a dependency ships.
Identical files are printed once with the crates that ship them.
"""

import hashlib
import json
import sys
from pathlib import Path

NAME_PREFIXES = ("license", "licence", "copying", "copyright", "notice", "unlicense")


def license_files(package_dir):
    files = []
    for entry in sorted(package_dir.iterdir()):
        if entry.is_file() and entry.name.lower().startswith(NAME_PREFIXES):
            files.append(entry)
    return files


def main(paths):
    crates = {}
    for path in paths:
        with open(path, encoding="utf-8") as report:
            for crate in json.load(report)["crates"]:
                package = crate["package"]
                manifest = Path(package["manifest_path"])
                # Workspace crates are covered by this repository's LICENSE.
                if "registry" not in manifest.parts and "git" not in manifest.parts:
                    continue
                crates[(package["name"], package["version"])] = manifest.parent

    texts = {}
    for (name, version), package_dir in sorted(crates.items()):
        for file in license_files(package_dir):
            text = file.read_bytes().decode("utf-8", errors="replace").strip()
            digest = hashlib.sha256(text.encode()).hexdigest()
            entry = texts.setdefault(digest, {"text": text, "users": []})
            entry["users"].append(f"{name} {version} ({file.name})")

    if not texts:
        sys.exit("no dependency license files found; check the cargo-about report")

    for entry in sorted(texts.values(), key=lambda entry: entry["users"][0]):
        print("Shipped by:\n")
        for user in entry["users"]:
            print(f"- {user}")
        print("\n```text")
        print(entry["text"].replace("```", "'''"))
        print("```\n")


if __name__ == "__main__":
    main(sys.argv[1:])
