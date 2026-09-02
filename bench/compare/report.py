import json, os, sys
S=sys.argv[1]; tools=["ours","intelephense","phpactor"]
# expectations: probe name -> dict(def=(file substring, 1-based line or None), refs=grep truth (int, 'decl+sites' note), completion=[expected labels])
EXP = {
 "gd-this-method-sendAsync": {"def": ("src/Client.php", 319), "refs": "10 sites + decl"},
 "gd-static-HandlerStack-create": {"def": ("src/HandlerStack.php", 56)},
 "gd-new-CookieJar": {"def": ("src/Cookie/CookieJar.php", None)},
 "gd-trait-this-request": {"def": ("src/ClientTrait.php", 110), "refs": "6 trait sites (+ Client.php impl 650, decl)"},
 "refs-CookieJar-count": {"refs": "decl + ? (grep ->count() finds 0)"},
 "refs-Client-ctor": {"refs": "304 `new Client(` + decl"},
 "gd-this-config-prop": {"def": ("src/Client.php", 37)},
 "rename-private-transfer": {"def": ("src/Client.php", None), "rename": "decl + 2 sites = 3 edits"},
 "gd-static-Utils-chooseHandler": {"def": ("src/Utils.php", 73)},
 "completion-this-arrow": {"completion": ["sendAsync", "transfer", "config", "getConfig", "handlers", "pushHandler", "addRecord"]},
 "gd-this-handlers-prop": {"def": ("src/Monolog/Logger.php", 134), "refs": "10 in Logger.php (+ subclasses?)"},
 "refs-pushHandler-decl": {"refs": "50 sites + decl"},
 "gd-parent-ctor": {"def": ("src/Monolog/Handler/AbstractHandler.php", 36)},
 "gd-static-Utils-canonicalizePath": {"def": ("src/Monolog/Utils.php", 47)},
 "refs-addRecord-decl": {"refs": "15 sites + decl"},
 "gd-use-leaf-HandlerInterface": {"def": ("src/Monolog/Handler/HandlerInterface.php", None)},
 "rename-this-url-prop": {"rename": "8 sites incl decl"},
 "gd-use-leaf-Post": {"def": ("src/Entity/Post.php", None)},
 "refs-Post-getTitle": {"refs": "4 in src (+ decl)"},
 "gd-param-PostRepository": {"def": ("src/Repository/PostRepository.php", None)},
}
res = {}
for r in ["guzzle","monolog","demo"]:
    for t in tools:
        p=f"{S}/cmp/res-{r}-{t}.json"
        if os.path.exists(p): res[(r,t)] = json.load(open(p))
print("## startup / footprint")
for r in ["guzzle","monolog","demo"]:
    for t in tools:
        d=res.get((r,t))
        if d: print(f"  {r:8} {t:13} ready {d['ready_s'] and round(d['ready_s'],1)} s   rss end {round((d.get('rss_end') or {}).get('rss',0))} MB")
for r in ["guzzle","monolog","demo"]:
    print(f"\n## {r}")
    names=[p["name"] for p in res[(r,tools[0])]["probes"]] if (r,tools[0]) in res else []
    for n in names:
        e=EXP.get(n,{}); print(f"\n### {n}  expected: {e}")
        for t in tools:
            d=res.get((r,t)); 
            if not d: print(f"  {t}: (no run)"); continue
            p=[x for x in d["probes"] if x["name"]==n][0]
            parts=[]
            if "definition" in p:
                locs=p["definition"]; ok=None
                if e.get("def"):
                    f,l=e["def"]; ok=any(f in str(x["file"]) and (l is None or x["line"]==l-1) for x in locs)
                parts.append(f"def={[(x['file'],(x['line'] or 0)+1) for x in locs][:4]} {'OK' if ok else ('MISS' if ok is False else '')} {round(p['definition_ms'])}ms")
            if "references" in p:
                fs={}
                for x in p["references"]: fs[x["file"]]=fs.get(x["file"],0)+1
                parts.append(f"refs={len(p['references'])} in {len(fs)} files {round(p['references_ms'])}ms")
            if "hover" in p:
                h=(p["hover"] or "").strip().replace("\n"," | ")[:110]; parts.append(f"hover={'∅' if not h else h!r}")
            if "completion" in p:
                want=e.get("completion",[]); have=set(p["completion"]); hit=[w for w in want if w in have]
                parts.append(f"completion={p['completion_count']} items, has {hit} {round(p['completion_ms'])}ms")
            if "rename" in p:
                fs={}
                for x in p["rename"]: fs[x["file"]]=fs.get(x["file"],0)+1
                parts.append(f"rename={len(p['rename'])} edits in {dict(fs)}" + (f" ERR={str(p['rename_error'])[:60]}" if p.get('rename_error') else ""))
            print(f"  {t:13} " + "  ".join(parts))
