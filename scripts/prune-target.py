#!/usr/bin/env python3
"""Leaves in target/ only what this run's builds used, for CI's cache.

A cache restored for an older Cargo.lock holds builds of crates that this
run built again under new hashes; cargo never removes the old ones, and the
cache saved at the end took them along, a layer more with every change of
the dependencies (2.6 GB on Linux, where a fresh one is 1.1). Cargo tells of
every unit it used, built or found fresh, with `--message-format=json`: the
files those runs wrote are read here, and a unit of target/ that none of
them names goes. Run with nothing told, it removes nothing.

    prune-target.py FILE...   cargo's JSON messages
    --drop CRATE,...          its builds go even when used: a crate of ours
                              that every commit changes is built again anyway
    --dry-run                 says what would go
"""
import json, os, re, shutil, sys

HASH = re.compile(r"^[0-9a-f]{16}$")
NAMED = re.compile(r"^.+-([0-9a-f]{16})(?:\..*)?$")


def used(paths):
    """The hashes of the units these messages name."""
    hashes = set()

    def take(path):
        for part in re.split(r"[\\/]", path):
            if HASH.match(part):
                hashes.add(part)
            elif m := NAMED.match(part):
                hashes.add(m.group(1))

    for path in paths:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                if not line.startswith("{"):
                    continue
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                for name in msg.get("filenames") or []:
                    take(name)
                for key in ("out_dir", "executable"):
                    if msg.get(key):
                        take(msg[key])
    return hashes


def crate_of(path, target):
    """The crate a unit of target/ is of, as cargo names it."""
    parts = os.path.relpath(path, target).split(os.sep)
    if len(parts) >= 2 and parts[-2] not in ("deps", ".fingerprint", "build"):
        return parts[-2]
    name = NAMED.match(parts[-1]).group(0).rsplit("-", 1)[0]
    return name[3:] if name.startswith("lib") else name


def units(target):
    """Each unit's place in target/, with its hash: build/<crate>/<hash> as
    cargo lays it out now, and <name>-<hash> in deps, build and .fingerprint
    as it did before."""
    for base, dirs, files in os.walk(target):
        name = os.path.basename(base)
        if name not in ("deps", ".fingerprint", "build", "incremental"):
            continue
        if name != "incremental":
            for entry in dirs + files:
                if m := NAMED.match(entry):
                    yield os.path.join(base, entry), m.group(1)
                elif name == "build" and entry in dirs:
                    inner = os.path.join(base, entry)
                    for unit in os.listdir(inner):
                        if HASH.match(unit):
                            yield os.path.join(inner, unit), unit
        dirs[:] = []


def main():
    args = sys.argv[1:]
    dry = "--dry-run" in args
    drop = set()
    if "--drop" in args:
        at = args.index("--drop")
        drop = {c.replace("-", "_") for c in args[at + 1].split(",")}
        del args[at : at + 2]
    told = [a for a in args if a != "--dry-run"]
    target = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target")
    keep = used([p for p in told if os.path.exists(p)])
    if not keep:
        print("prune-target: no unit told of, nothing removed")
        return
    gone, freed = 0, 0
    for path, digest in list(units(target)):
        if digest in keep and crate_of(path, target).replace("-", "_") not in drop:
            continue
        size = 0
        if os.path.isdir(path) and not os.path.islink(path):
            size = sum(os.lstat(os.path.join(b, f)).st_size for b, _, fs in os.walk(path) for f in fs)
            if not dry:
                shutil.rmtree(path, ignore_errors=True)
        else:
            size = os.lstat(path).st_size
            if not dry:
                os.remove(path)
        gone += 1
        freed += size
    verb = "would remove" if dry else "removed"
    print(f"prune-target: {len(keep)} units used, {verb} {gone} others, {freed / 2**20:.0f} MB")


if __name__ == "__main__":
    main()
