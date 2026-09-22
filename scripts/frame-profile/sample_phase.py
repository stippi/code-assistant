#!/usr/bin/env python3
"""Attribute a macOS `sample` call graph of the main thread to draw phases.

Demangle the capture first (`c++filt` handles Rust v0 symbols):

    sample <pid> 8 -file raw.txt && c++filt < raw.txt > out.txt

Walks the main thread's tree below `Window::draw` and assigns every self
sample to the innermost frame whose symbol matches a phase rule. Also
reports inclusive counts for symbols of interest (--incl a,b,c: samples with
that substring anywhere on the stack; note that the window root's own
`prepaint_as_root` is on every stack) and the top self symbols.

    sample_phase.py out.txt [--incl a,b] [--top N]

--tree SYM prints the merged subtree below every node matching SYM instead,
with single-child chains collapsed (--depth D, --min COUNT).

Attribution is heuristic: check the top self symbols and a subtree before
trusting a number, see docs/frame-profiling.md.
"""
import re
import sys
from collections import Counter

LINE = re.compile(
    r"^([ +!:|]*?)(\d+) (.*?)(?:\s+\(in ([^)]*)\))?(?:\s+\+\s+\d+)?"
    r"(?:\s+\[0x[0-9a-f]+(?:,0x[0-9a-f]+)?\])?\s*$"
)

# Innermost known frame wins; the order here only breaks ties within a frame.
PHASES = [
    ("present/metal", [r"MTL", r"Metal", r"present", r"CAMetalLayer", r"nextDrawable"]),
    ("taffy layout", [r"taffy", r"compute_layout", r"LayoutEngine", r"to_taffy", r"round_layout"]),
    ("text shaping", [r"layout_line", r"shape_line", r"shape_text", r"CTLine", r"CTTypesetter", r"CoreText", r"CTFont", r"LineLayoutCache", r"MacTextSystem"]),
    ("text wrapping", [r"LineWrapper", r"wrap_line", r"line_wrapper", r"is_word_char", r"line_ranges", r"push_text_wrap_fragments"]),
    ("tree-sitter", [r"tree_sitter|ts_query|ts_tree|highlight"]),
    ("paint", [r"paint_glyph", r"paint_quad", r"paint_shadows", r"paint_path", r"rasterize", r"glyph_atlas|GlyphAtlas|Atlas", r"Scene::", r"insert_primitive", r"::paint", r"paint_"]),
    ("prepaint/hitbox", [r"insert_hitbox", r"prepaint", r"BoundsTree", r"DispatchTree", r"with_absolute_element_offset", r"layout_bounds", r"content_mask"]),
    ("markdown/textview build", [r"text_view", r"TextView", r"node::", r"NodeContext", r"markdown", r"InlineFlow|inline_flow", r"Inline::|inline::"]),
    ("diff card build", [r"diff_rows|DiffRows|diff_card|tool_cards"]),
    ("event dispatch", [r"dispatch_event", r"dispatch_mouse", r"on_mouse_event", r"ScrollWheel", r"handle_input", r"mouse_listeners"]),
    ("list bookkeeping", [r"ListState|list::|List<|list_state", r"virtual_list|VirtualList"]),
    ("element build (render/request_layout)", [r"::render", r"request_layout", r"into_any_element", r"IntoElement", r"Div::", r"div::", r"Styled", r"StyleRefinement", r"Style::", r"with_element_state", r"GlobalElementId", r"ElementId", r"Interactivity"]),
    ("alloc/free", [r"malloc|free|realloc|calloc|_free|szone|nano_", r"memmove|memcpy|memset|bzero", r"drop_in_place", r"RawVec|alloc::"]),
    ("hash", [r"hash|Hash|FxHash|SipHash|siphash|HashMap|hashbrown"]),
]
COMPILED = [(name, [re.compile(p) for p in pats]) for name, pats in PHASES]


def parse(path):
    lines = open(path, errors="replace").read().splitlines()
    try:
        start = next(i for i, l in enumerate(lines) if l.startswith("Call graph:"))
    except StopIteration:
        sys.exit("no call graph in " + path)
    stack = []
    roots = []
    seen = False
    for l in lines[start + 1 :]:
        if not l.strip():
            if seen:
                break
            continue
        if l.startswith(("Total number in stack", "Sort by", "Binary Images")):
            break
        m = LINE.match(l)
        if not m:
            continue
        seen = True
        node = {"sym": m[3].strip(), "count": int(m[2]), "children": [], "indent": len(m[1])}
        while stack and stack[-1]["indent"] >= node["indent"]:
            stack.pop()
        (stack[-1]["children"] if stack else roots).append(node)
        stack.append(node)
    return roots


def find(node, pred):
    if pred(node):
        return node
    for c in node["children"]:
        r = find(c, pred)
        if r:
            return r
    return None


def find_all(node, pred, out):
    if pred(node):
        out.append(node)
        return
    for c in node["children"]:
        find_all(c, pred, out)


def classify(sym):
    for name, pats in COMPILED:
        if any(p.search(sym) for p in pats):
            return name
    return None


def self_count(node):
    return node["count"] - sum(c["count"] for c in node["children"])


def walk(node, path, phases, incl, incl_hits, selfs):
    path = path + [node["sym"]]
    own = self_count(node)
    if own > 0:
        phase = next((p for p in (classify(s) for s in reversed(path)) if p), "unclassified")
        phases[phase] += own
        selfs[node["sym"]] += own
        for name in incl:
            if any(name in s for s in path):
                incl_hits[name] += own
    for c in node["children"]:
        walk(c, path, phases, incl, incl_hits, selfs)


def main_thread(roots):
    for r in roots:
        if "Thread_" in r["sym"] and "main-thread" in r["sym"]:
            return r
    return max(roots, key=lambda r: r["count"])


def short(sym, n=95):
    sym = re.sub(r"::\{closure#\d+\}", "{c}", sym)
    for prefix in ("gpui::element::", "gpui::elements::div::", "gpui::"):
        sym = sym.replace(prefix, "")
    return sym[:n]


def print_tree(node, depth, max_depth, min_count):
    chain = [node]
    while len(chain[-1]["children"]) == 1 and chain[-1]["children"][0]["count"] == chain[-1]["count"]:
        chain.append(chain[-1]["children"][0])
    last = chain[-1]
    label = short(node["sym"]) if len(chain) == 1 else short(node["sym"], 60) + " .. " + short(last["sym"], 60)
    print(f"{'  ' * depth}{node['count']:>6} {label}")
    if depth >= max_depth:
        return
    for c in sorted(last["children"], key=lambda c: -c["count"]):
        if c["count"] >= min_count:
            print_tree(c, depth + 1, max_depth, min_count)


def option(args, name, default, conv=str):
    return conv(args[args.index(name) + 1]) if name in args else default


def main():
    args = sys.argv[1:]
    paths = [a for a in args if a.endswith(".txt")]
    if not paths:
        sys.exit(__doc__)
    if "--tree" in args:
        sym = option(args, "--tree", "")
        depth = option(args, "--depth", 12, int)
        min_count = option(args, "--min", 10, int)
        for path in paths:
            hits = []
            find_all(main_thread(parse(path)), lambda n: sym in n["sym"], hits)
            print(f"== {path}: {len(hits)} node(s) matching {sym!r}, total={sum(h['count'] for h in hits)}")
            merged = {"sym": sym, "count": sum(h["count"] for h in hits), "children": [c for h in hits for c in h["children"]]}
            print_tree(merged, 0, depth, min_count)
        return

    incl = option(args, "--incl", "").split(",") if "--incl" in args else []
    top = option(args, "--top", 30, int)
    for path in paths:
        thread = main_thread(parse(path))
        draw = find(thread, lambda n: "Window>::draw" in n["sym"] or "Window::draw" in n["sym"])
        total = thread["count"]
        if draw is None:
            print(f"== {path}: main thread samples={total} (no Window::draw found; classifying the whole thread)")
            draw = thread
        else:
            print(f"== {path}: main thread samples={total}, Window::draw samples={draw['count']} ({100 * draw['count'] / total:.0f}% of thread)")
        phases, incl_hits, selfs = Counter(), Counter(), Counter()
        walk(draw, [], phases, incl, incl_hits, selfs)
        d = draw["count"]
        for name, c in phases.most_common():
            print(f"  {name:<40} {c:>7} {100 * c / d:>5.1f}%")
        if incl:
            print("  -- inclusive (samples with the symbol on the stack, within draw):")
            for name in incl:
                print(f"  {name:<40} {incl_hits[name]:>7} {100 * incl_hits[name] / d:>5.1f}%")
        print(f"  -- top {top} self symbols in draw:")
        for sym, c in selfs.most_common(top):
            print(f"  {c:>7} {100 * c / d:>5.1f}%  {sym[:110]}")


if __name__ == "__main__":
    main()
