#!/usr/bin/env python3
import argparse
import json
import sys
import urllib.request
from pathlib import Path

USER_AGENT = "crate-backup (https://github.com/rust-lang/crates.io)"


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    return urllib.request.urlopen(req)


def list_versions(crate):
    url = f"https://crates.io/api/v1/crates/{crate}/versions"
    with fetch(url) as resp:
        data = json.load(resp)
    return [v["num"] for v in data["versions"]]


def download_version(crate, version):
    url = f"https://static.crates.io/crates/{crate}/{crate}-{version}.crate"
    dest = Path.cwd() / f"{crate}-{version}.crate"
    if dest.exists():
        print(f"skip {dest.name} (already exists)")
        return
    print(f"download {dest.name}")
    with fetch(url) as resp, open(dest, "wb") as f:
        while chunk := resp.read(64 * 1024):
            f.write(chunk)


def main():
    parser = argparse.ArgumentParser(description="Download all versions of one or more crates.")
    parser.add_argument("crates", nargs="+", metavar="CRATE", help="Name of a crate")
    args = parser.parse_args()

    for crate in args.crates:
        versions = list_versions(crate)
        print(f"found {len(versions)} versions of {crate}")
        for version in versions:
            download_version(crate, version)


if __name__ == "__main__":
    try:
        main()
    except urllib.error.HTTPError as e:
        print(f"HTTP error: {e}", file=sys.stderr)
        sys.exit(1)
