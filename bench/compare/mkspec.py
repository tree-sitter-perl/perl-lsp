import json, sys, os
# spec rows: (name, file, line1, token, occurrence(1-based), verbs, new_name)
def build(root, readiness, rows):
    def pos(file, line1, token, occ):
        line = open(os.path.join(root, file), encoding="utf8", errors="replace").read().split("\n")[line1-1]
        idx = -1
        for _ in range(occ):
            idx = line.index(token, idx+1)
        return idx
    probes = []
    for name, file, line1, token, occ, verbs, *rest in rows:
        probes.append({"name": name, "file": file, "line": line1-1, "character": pos(file, line1, token, occ), "verbs": verbs, "new_name": (rest[0] if rest else "zzRenamed"), "token": token})
    rf, rl, rt = readiness
    return {"readiness": {"file": rf, "line": rl-1, "character": pos(rf, rl, rt, 1)}, "probes": probes}
S=sys.argv[1]; C=S+"/php-corpus"
specs = {
 "guzzle": build(C+"/guzzle", ("src/Client.php", 422, "sendAsync"), [
  ("gd-this-method-sendAsync", "src/Client.php", 422, "sendAsync", 1, ["definition","hover","references"]),
  ("gd-static-HandlerStack-create", "src/Client.php", 196, "create", 1, ["definition"]),
  ("gd-new-CookieJar", "src/Client.php", 885, "CookieJar", 1, ["definition"]),
  ("gd-trait-this-request", "src/ClientTrait.php", 202, "request", 1, ["definition","references"]),
  ("refs-CookieJar-count", "src/Cookie/CookieJar.php", 239, "count", 1, ["references","hover"]),
  ("refs-Client-ctor", "src/Client.php", 163, "__construct", 1, ["references"]),
  ("gd-this-config-prop", "src/Client.php", 676, "config", 1, ["definition","hover"]),
  ("rename-private-transfer", "src/Client.php", 328, "transfer", 1, ["definition","rename"], "transferZ"),
  ("gd-static-Utils-chooseHandler", "src/Client.php", 197, "chooseHandler", 1, ["definition"]),
  ("completion-this-arrow", "src/Client.php", 676, "config", 1, ["completion"]),
 ]),
 "monolog": build(C+"/monolog", ("src/Monolog/Logger.php", 209, "handlers"), [
  ("gd-this-handlers-prop", "src/Monolog/Logger.php", 209, "handlers", 1, ["definition","references","hover"]),
  ("refs-pushHandler-decl", "src/Monolog/Logger.php", 207, "pushHandler", 1, ["references","hover"]),
  ("gd-parent-ctor", "src/Monolog/Handler/StreamHandler.php", 53, "__construct", 1, ["definition"]),
  ("gd-static-Utils-canonicalizePath", "src/Monolog/Handler/StreamHandler.php", 73, "canonicalizePath", 1, ["definition"]),
  ("refs-addRecord-decl", "src/Monolog/Logger.php", 332, "addRecord", 1, ["references","hover"]),
  ("gd-use-leaf-HandlerInterface", "src/Monolog/Logger.php", 17, "HandlerInterface", 1, ["definition"]),
  ("completion-this-arrow", "src/Monolog/Logger.php", 251, "handlers", 1, ["completion"]),
  ("rename-this-url-prop", "src/Monolog/Handler/StreamHandler.php", 73, "url", 1, ["rename"], "urlZ"),
 ]),
 "demo": build(C+"/demo", ("src/Controller/BlogController.php", 15, "Post"), [
  ("gd-use-leaf-Post", "src/Controller/BlogController.php", 15, "Post", 1, ["definition"]),
  ("refs-Post-getTitle", "src/Entity/Post.php", 93, "getTitle", 1, ["references","hover"]),
  ("gd-param-PostRepository", "src/Controller/BlogController.php", 52, "PostRepository", 1, ["definition"]),
 ]),
}
for k, v in specs.items():
    json.dump(v, open(f"{S}/cmp/spec-{k}.json", "w"), indent=1)
    print(k, [(p["name"], p["line"], p["character"]) for p in v["probes"]][:3], "...")
