#!/usr/bin/env python3
"""Resolves a merge conflict in crates/coolify-api/src/routes/mod.rs by union.

Branches add entries to the same two tables (ROUTES and the handlers list), which git sees as
conflicts. Run during a conflicted merge: it keeps our file and adds their new `pub mod`, `use`
and table entries, then `git add`s the file. Review with `cargo build`.
"""
import re
import subprocess

P = "crates/coolify-api/src/routes/mod.rs"
show = lambda stage: subprocess.check_output(["git", "show", f":{stage}:{P}"], text=True)
ours, theirs = show(2), show(3)


def route_blocks(s):
    body = s[s.index("pub const ROUTES"):]
    body = body[:body.index("\n];")]
    return re.findall(r"    RouteSpec \{\n.*?\n    \},\n", body + "\n", re.S)


def route_key(b):
    return re.search(r'method: "(\w+)"', b)[1], re.search(r'uri: "([^"]+)"', b)[1]


def handler_items(s):
    body = s[s.index("MethodRouter<AppState>)> = vec!["):]
    body = body[body.index("\n") + 1:body.index("\n    ];")]
    items, cur = [], []
    for line in body.split("\n"):
        if cur:
            cur.append(line)
            if line.startswith("        ),"):
                items.append("\n".join(cur))
                cur = []
        elif line.startswith("        (") and line.rstrip().endswith("),"):
            items.append(line)
        elif line.strip() == "(":
            cur = [line]
    return items


def handler_key(item):
    return tuple(re.findall(r'"([^"]+)"', item)[:2])


have = {route_key(b) for b in route_blocks(ours)}
extra = "".join(b for b in route_blocks(theirs) if route_key(b) not in have)
out = ours.replace("\n];", "\n" + extra.rstrip("\n") + "\n];", 1) if extra else ours
have = {handler_key(i) for i in handler_items(ours)}
extra = [i for i in handler_items(theirs) if handler_key(i) not in have]
if extra:
    out = out.replace("\n    ];", "\n" + "\n".join(extra) + "\n    ];", 1)
for line in theirs.split("\n"):
    if re.match(r"(pub )?(mod|use) ", line) and line not in out:
        out = out.replace("\nuse axum::Router;", f"\n{line}\nuse axum::Router;", 1) if line.startswith("pub mod") or line.startswith("mod") else out.replace("\npub const ROUTES", f"\n{line}\npub const ROUTES", 1)
open(P, "w").write(out)
subprocess.check_call(["git", "add", P])
print(f"merged {P}")
