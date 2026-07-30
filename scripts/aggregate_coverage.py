#!/usr/bin/env python3
"""Aggregate cargo-llvm-cov JSON export into per-crate / per-module line coverage."""
import json, os, sys, subprocess
from collections import defaultdict

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

with open(os.path.join(REPO, "coverage.json")) as f:
    data = json.load(f)

# llvm-cov export format: data["data"][0]["files"] = [{filename, summary:{lines:{count,covered,percent}, functions, regions}}]
files = data["data"][0]["files"]

def rel(fn):
    p = os.path.relpath(fn, REPO)
    return p

# crate -> module -> [covered_lines, total_lines, covered_fns, total_fns, files]
crate_mod = defaultdict(lambda: defaultdict(lambda: [0, 0, 0, 0, 0]))
crate_files = defaultdict(list)

skipped = 0
for entry in files:
    fn = entry["filename"]
    p = rel(fn)
    if not p.startswith("crates/"):
        skipped += 1
        continue
    parts = p.split("/")
    # crates/<crate>/src/<module...>/file.rs
    crate = parts[1]
    if len(parts) >= 4 and parts[2] == "src":
        rest = parts[3:]
        if len(rest) == 1:
            module = "(crate root)"
        else:
            module = rest[0]
    elif len(parts) >= 4 and parts[2] in ("tests", "benches", "examples"):
        module = f"({parts[2]})"
    else:
        module = "(other)"

    s = entry["summary"]
    lines = s["lines"]
    fns = s["functions"]
    lc, lt = lines["covered"], lines["count"]
    fc, ft = fns["covered"], fns["count"]

    m = crate_mod[crate][module]
    m[0] += lc; m[1] += lt; m[2] += fc; m[3] += ft; m[4] += 1
    crate_files[crate].append({
        "path": p,
        "lines_covered": lc, "lines_total": lt,
        "line_pct": lines["percent"],
        "fns_covered": fc, "fns_total": ft,
    })

# test counts per crate (from grep)
def test_counts():
    counts = {}
    for crate in os.listdir(os.path.join(REPO, "crates")):
        cdir = os.path.join(REPO, "crates", crate)
        if not os.path.isdir(cdir):
            continue
        n = 0
        for root, _, fs in os.walk(cdir):
            if "/target/" in root:
                continue
            for fname in fs:
                if fname.endswith(".rs"):
                    try:
                        with open(os.path.join(root, fname), errors="ignore") as fh:
                            for line in fh:
                                l = line.strip()
                                if l in ("#[test]", "#[tokio::test]") or l.startswith("#[tokio::test") or l.startswith("#[test_case") or l.startswith("#[rstest"):
                                    n += 1
                    except Exception:
                        pass
        counts[crate] = n
    return counts

tests = test_counts()

out = {"crates": [], "grand": {}}
gc = gt = gfc = gft = 0
for crate in sorted(crate_mod):
    mods = []
    clc = clt = cfc = cft = 0
    for module in sorted(crate_mod[crate]):
        lc, lt, fc, ft, nf = crate_mod[crate][module]
        clc += lc; clt += lt; cfc += fc; cft += ft
        mods.append({
            "module": module,
            "lines_covered": lc, "lines_total": lt,
            "line_pct": round(100 * lc / lt, 1) if lt else None,
            "fns_covered": fc, "fns_total": ft,
            "fn_pct": round(100 * fc / ft, 1) if ft else None,
            "files": nf,
        })
    gc += clc; gt += clt; gfc += cfc; gft += cft
    out["crates"].append({
        "crate": crate,
        "lines_covered": clc, "lines_total": clt,
        "line_pct": round(100 * clc / clt, 1) if clt else None,
        "fns_covered": cfc, "fns_total": cft,
        "fn_pct": round(100 * cfc / cft, 1) if cft else None,
        "tests": tests.get(crate, 0),
        "modules": sorted(mods, key=lambda m: (m["lines_total"] == 0, m["module"])),
    })

out["grand"] = {
    "lines_covered": gc, "lines_total": gt,
    "line_pct": round(100 * gc / gt, 1) if gt else None,
    "fns_covered": gfc, "fns_total": gft,
    "fn_pct": round(100 * gfc / gft, 1) if gft else None,
    "tests_total": sum(tests.values()),
    "crate_count": len(out["crates"]),
}
out["skipped_files"] = skipped

with open(os.path.join(REPO, "coverage_summary.json"), "w") as f:
    json.dump(out, f, indent=2)

print(f"crates={len(out['crates'])} grand_line_pct={out['grand']['line_pct']} tests={out['grand']['tests_total']} skipped={skipped}")
