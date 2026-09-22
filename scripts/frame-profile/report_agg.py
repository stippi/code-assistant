#!/usr/bin/env python3
"""Aggregate the `[frame-profile]` reports of a code-assistant stderr log.

Frame-weighted over the intervals with scroll=yes (or every interval with
--all): draw mean, p50 and p95 (frame-weighted means of the interval
values), the max over intervals, and per label the ms per frame and
n/frame. See docs/frame-profiling.md.

Usage: report_agg.py LOG... [--all] [--max-mean MS]

--max-mean drops intervals whose draw mean is above MS, e.g. the ones with
`sample` attached.
"""
import re
import sys

HEAD = re.compile(
    r"\[frame-profile\] t=([\d.]+)s scroll=(yes|no) frames=(\d+) draw/frame "
    r"mean=([\d.]+)ms p50=([\d.]+)ms p95=([\d.]+)ms max=([\d.]+)ms"
)
ROW = re.compile(
    r"^\s{2}(\S.*?)\s{2,}([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+(\d+)%$"
)
DERIVED = re.compile(r"^\s{2}row layout compute \(derived\)\s+([\d.]+)\s+(\d+)%$")


def parse(path):
    intervals = []
    cur = None
    for line in open(path, errors="replace"):
        m = HEAD.match(line.strip())
        if m:
            cur = {
                "t": float(m[1]),
                "scroll": m[2] == "yes",
                "frames": int(m[3]),
                "mean": float(m[4]),
                "p50": float(m[5]),
                "p95": float(m[6]),
                "max": float(m[7]),
                "labels": {},
                "derived": None,
            }
            intervals.append(cur)
            continue
        if cur is None:
            continue
        m = DERIVED.match(line.rstrip("\n"))
        if m:
            cur["derived"] = float(m[1])
            continue
        m = ROW.match(line.rstrip("\n"))
        if m and m[1] != "label (ms per frame)":
            cur["labels"][m[1]] = tuple(float(x) for x in m.groups()[1:7])
    return intervals


def aggregate(intervals, max_mean=None):
    sel = [i for i in intervals if i["frames"] > 0]
    if max_mean is not None:
        sel = [i for i in sel if i["mean"] <= max_mean]
    frames = sum(i["frames"] for i in sel)
    if frames == 0:
        return None
    weighted = lambda key: sum(i[key] * i["frames"] for i in sel) / frames
    labels = {}
    for i in sel:
        for name, vals in i["labels"].items():
            acc = labels.setdefault(name, [0.0] * 6)
            for k, v in enumerate(vals):
                acc[k] += v * i["frames"]
    for name in labels:
        labels[name] = [v / frames for v in labels[name]]
    derived = [i for i in sel if i["derived"] is not None]
    dframes = sum(i["frames"] for i in derived)
    return {
        "intervals": len(sel),
        "frames": frames,
        "mean": weighted("mean"),
        "p50": weighted("p50"),
        "p95": weighted("p95"),
        "max": max(i["max"] for i in sel),
        "labels": labels,
        "derived": (sum(i["derived"] * i["frames"] for i in derived) / dframes)
        if dframes
        else None,
    }


def main():
    args = sys.argv[1:]
    use_all = "--all" in args
    max_mean = None
    if "--max-mean" in args:
        max_mean = float(args[args.index("--max-mean") + 1])
    paths = [a for a in args if not a.startswith("--") and not re.fullmatch(r"[\d.]+", a)]
    for path in paths:
        intervals = parse(path)
        sel = intervals if use_all else [i for i in intervals if i["scroll"]]
        agg = aggregate(sel, max_mean)
        print(f"== {path}")
        if not agg:
            print("  no frames")
            continue
        print(
            f"  intervals={agg['intervals']} frames={agg['frames']} "
            f"draw mean={agg['mean']:.2f}ms p50={agg['p50']:.2f}ms "
            f"p95={agg['p95']:.2f}ms max={agg['max']:.2f}ms"
        )
        print(
            f"  {'label':<34} {'n/frame':>8} {'build':>8} {'layout':>8} "
            f"{'prepaint':>8} {'paint':>8} {'total':>8} {'%draw':>6}"
        )
        for name, v in sorted(agg["labels"].items(), key=lambda kv: -kv[1][5]):
            print(
                f"  {name:<34} {v[0]:>8.2f} {v[1]:>8.2f} {v[2]:>8.2f} {v[3]:>8.2f} "
                f"{v[4]:>8.2f} {v[5]:>8.2f} {100 * v[5] / agg['mean']:>5.0f}%"
            )
        if agg["derived"] is not None:
            print(
                f"  {'row layout compute (derived)':<34} {'':>8} {'':>8} {'':>8} "
                f"{'':>8} {'':>8} {agg['derived']:>8.2f} "
                f"{100 * agg['derived'] / agg['mean']:>5.0f}%"
            )


if __name__ == "__main__":
    main()
