"""End-to-end smoke test: runs two Lanclip services on this machine and drives them through
the CLI and HTTP API. Usage: python test/smoke.py [path/to/lanclip binary]"""
import json, os, shutil, subprocess, sys, tempfile, time, urllib.error, urllib.parse, urllib.request
from pathlib import Path

BIN = Path(sys.argv[1] if len(sys.argv) > 1 else ("target/release/lanclip.exe" if os.name == "nt" else "target/release/lanclip")).resolve()
KEYS = {18101: "ALPHAKEY0001", 18201: "BETAKEY00002"}
tmp = Path(tempfile.mkdtemp(prefix="lanclip-test-"))
procs = []


def env_for(name, port):
    e = dict(os.environ, LANCLIP_DATA=str(tmp / name), LANCLIP_PORT=str(port), LANCLIP_DISCOVERY_PORT="41999",
             LANCLIP_NO_INTEGRATION="1", LANCLIP_NO_NOTIFY="1", LANCLIP_NO_PROMPT="1")
    return e


def start(name, port, peer_port):
    d = tmp / name
    d.mkdir(parents=True)
    (d / "settings.json").write_text(json.dumps({"id": f"{name}-0000-test", "name": name, "key": KEYS[port], "manualPeers": [f"127.0.0.1:{peer_port}"]}))
    procs.append(subprocess.Popen([str(BIN), "daemon"], env=env_for(name, port), stderr=subprocess.DEVNULL))


def api(port, path, method="GET", body=None, headers=None, key=None):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=body, method=method, headers={"x-lanclip-key": key or KEYS[port], **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def state(port):
    return json.loads(api(port, "/api/state")[1])


def wait_for(fn, what, secs=10):
    end = time.time() + secs
    while time.time() < end:
        try:
            if fn():
                return
        except Exception:
            pass
        time.sleep(0.15)
    raise AssertionError(f"timed out waiting for {what}")


def cli(name, port, *args, stdin=None):
    subprocess.run([str(BIN), *args], env=env_for(name, port), input=stdin, timeout=30, check=True)


def ok(msg):
    print("ok ", msg)


try:
    A, B = 18101, 18201
    start("alpha", A, B)
    start("beta", B, A)
    def dev(port, name):
        return next((p for p in state(port)["peers"] if p["name"] == name), None)

    def post_json(port, path, body):
        return api(port, path, "POST", json.dumps(body).encode(), {"content-type": "application/json"})

    wait_for(lambda: dev(A, "beta") and dev(B, "alpha"), "devices see each other")
    assert not dev(A, "beta")["paired"]
    ok("devices discovered, not paired yet")

    # Request -> decline
    status, body = post_json(A, "/api/local/pair", {"id": dev(A, "beta")["id"]})
    assert status == 200, body
    code = json.loads(body)["code"]
    wait_for(lambda: dev(B, "alpha")["pending"] == "incoming", "incoming request")
    assert dev(B, "alpha")["code"] == code and dev(A, "beta")["pending"] == "outgoing"
    post_json(B, "/api/local/pair-respond", {"id": dev(B, "alpha")["id"], "accept": False})
    wait_for(lambda: dev(A, "beta")["pending"] == "" and not dev(A, "beta")["paired"], "decline reaches alpha")
    assert not dev(B, "alpha")["paired"]
    ok("pair request -> decline")

    # Request -> accept: keys are exchanged automatically
    post_json(A, "/api/local/pair", {"id": dev(A, "beta")["id"]})
    wait_for(lambda: dev(B, "alpha")["pending"] == "incoming", "incoming request 2")
    post_json(B, "/api/local/pair-respond", {"id": dev(B, "alpha")["id"], "accept": True})
    wait_for(lambda: dev(A, "beta")["paired"] and dev(B, "alpha")["paired"], "paired both ways")
    ok("pair request -> accept, paired both ways")

    # Unpaired outsiders can't use the API or fake an accept
    assert api(A, "/api/state", key="WRONG")[0] == 401
    assert api(A, "/api/pair/accept", "POST", json.dumps({"id": "x", "key": "y" * 10}).encode(), key="WRONG")[0] == 401

    assert api(A, "/api/state", key="WRONG")[0] == 401
    ok("wrong key rejected")

    status, _ = api(A, "/api/items", "POST", "héllo 👋".encode(), {"x-lanclip-meta": urllib.parse.quote(json.dumps({"kind": "text"}))})
    assert status == 200
    wait_for(lambda: state(B)["items"][0].get("text") == "héllo 👋", "text on beta")
    ok("text relayed")

    blob = os.urandom(3 * 1024 * 1024)
    api(A, "/api/items", "POST", blob, {"x-lanclip-meta": urllib.parse.quote(json.dumps({"kind": "file", "name": "big file.bin"}))})
    wait_for(lambda: state(B)["items"][0]["name"] == "big file.bin", "file on beta")
    item = state(B)["items"][0]
    assert api(B, f"/api/items/{item['id']}/content?download=1")[1] == blob
    ok("3 MB file relayed intact + download")

    time.sleep(0.5)
    assert len(state(A)["items"]) == 2 and len(state(B)["items"]) == 2
    ok("no echo / duplicates")

    # Context-menu path: CLI "send" on alpha (folder + file), CLI "paste" on beta
    src = tmp / "src"
    (src / "Photos" / "trip").mkdir(parents=True)
    (src / "Photos" / "a.jpg").write_text("AAA")
    (src / "Photos" / "trip" / "b.txt").write_text("BBB")
    (src / "notes.md").write_text("# hi")
    cli("alpha", A, "send", str(src / "Photos"))
    cli("alpha", A, "send", str(src / "notes.md"))  # separate launch, like Explorer multi-select
    dest = tmp / "dest"
    (dest / "Photos").mkdir(parents=True)
    (dest / "notes.md").write_text("old")
    wait_for(lambda: len([i for i in state(B)["items"] if i["batch"] == state(B)["items"][0]["batch"]]) == 3, "batch on beta")
    cli("beta", B, "paste", str(dest))
    wait_for(lambda: (dest / "notes (2).md").read_text() == "# hi" and (dest / "Photos (2)" / "a.jpg").read_text() == "AAA"
             and (dest / "Photos (2)" / "trip" / "b.txt").read_text() == "BBB", "paste")
    assert (dest / "notes.md").read_text() == "old"
    ok("CLI send (2 launches -> 1 batch) -> paste on other device, structure kept, nothing overwritten")

    cli("alpha", A, "send-text", stdin=b"from stdin")
    wait_for(lambda: state(B)["items"][0].get("text") == "from stdin", "send-text")
    cli("beta", B, "paste", str(dest))
    wait_for(lambda: (dest / "LAN text.txt").read_text() == "from stdin", "text paste")
    ok("send-text + paste as .txt")

    meta = {"id": "evil-item-0001", "kind": "file", "name": "x", "relPath": "../../../escape/x", "batch": "evilbatch1", "origin": {"id": "z", "name": "z"}}
    api(B, "/api/items", "POST", b"x", {"x-lanclip-meta": urllib.parse.quote(json.dumps(meta)), "x-lanclip-peer": "z"})
    cli("beta", B, "paste", str(dest))
    wait_for(lambda: (dest / "escape").exists(), "evil paste")
    assert not (tmp / "escape").exists()
    ok("relPath traversal contained in paste folder")

    post_json(A, "/api/local/unpair", {"id": dev(A, "beta")["id"]})
    wait_for(lambda: not dev(A, "beta")["paired"] and not dev(B, "alpha")["paired"], "unpaired both ways")
    n = len(state(B)["items"])
    cli("alpha", A, "send-text", stdin=b"after unpair")
    time.sleep(1.5)
    assert len(state(B)["items"]) == n
    ok("disconnect removes pairing on both sides; nothing is sent afterwards")

    print("\nall smoke tests passed")
finally:
    for p in procs:
        p.kill()
    time.sleep(0.5)
    shutil.rmtree(tmp, ignore_errors=True)
