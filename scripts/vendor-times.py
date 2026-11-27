#!/usr/bin/env python3
"""vendor/'s crates taken from CI's cache when they are what was built.

Cargo tells a local crate fresh by its files' times, and a checkout makes
them all new: every run built vendor/ again, optimized, minutes a crate.
After a build `record` writes what each crate of vendor/ held (a hash of
its files) beside the cache; before the next one `old` makes the files of
each crate that still holds the same older than any build, so cargo takes
its build from the cache. A crate that changed keeps its new times and is
built again, with what depends on it; the others are not.

    vendor-times.py old DIR      DIR: what the cache brought back
    vendor-times.py record DIR
"""
import hashlib, json, os, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
VENDOR = os.path.join(ROOT, "vendor")
OLD = 946684800  # 2000-01-01


def files(crate):
    for base, dirs, names in os.walk(crate):
        dirs.sort()
        for name in sorted(names):
            yield os.path.join(base, name)


def held(crate):
    digest = hashlib.sha256()
    for path in files(crate):
        digest.update(os.path.relpath(path, crate).replace(os.sep, "/").encode())
        with open(path, "rb") as f:
            digest.update(hashlib.sha256(f.read()).digest())
    return digest.hexdigest()


def crates():
    return {name: held(os.path.join(VENDOR, name)) for name in sorted(os.listdir(VENDOR)) if os.path.isdir(os.path.join(VENDOR, name))}


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("old", "record"):
        sys.exit(__doc__)
    what, dir = sys.argv[1], sys.argv[2]
    manifest = os.path.join(dir, "vendor.json")
    now = crates()
    if what == "record":
        os.makedirs(dir, exist_ok=True)
        with open(manifest, "w") as f:
            json.dump(now, f, indent=1)
        print(f"vendor-times: {len(now)} crates recorded")
        return
    try:
        with open(manifest) as f:
            built = json.load(f)
    except (OSError, ValueError):
        built = {}
    same = [name for name, digest in now.items() if built.get(name) == digest]
    for name in same:
        for path in files(os.path.join(VENDOR, name)):
            os.utime(path, (OLD, OLD))
    print(f"vendor-times: from the cache {', '.join(same) or 'none'}; built again {', '.join(sorted(set(now) - set(same))) or 'none'}")


if __name__ == "__main__":
    main()
