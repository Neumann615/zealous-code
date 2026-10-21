# -*- coding: utf-8 -*-
"""Provider fault mock for spec 151 A2.

Usage: python a2_mock.py <mode> [port] [n_fail]
Modes:
  always500    - every request -> HTTP 500
  badjson      - 200 with non-JSON body
  hang         - accept and never respond (client timeout)
  failn_then_ok- first n_fail requests -> 500, then scripted success
                 (plan -> edit_file note.txt -> message)
"""
import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODE = sys.argv[1]
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 19101
N_FAIL = int(sys.argv[3]) if len(sys.argv) > 3 else 6

LOCK = threading.Lock()
COUNT = [0]
OK_IDX = [0]
ACTIONS = [
    {"action": "plan", "plan": "fix note.txt", "workers": []},
    {"action": "tool", "tool": "edit_file",
     "args": {"path": "note.txt", "old_text": "ACCEPT", "new_text": "PASSED"}},
    {"action": "message", "text": "a2 recovery done"},
]


def respond(handler, code, body):
    data = body.encode("utf-8")
    handler.send_response(code)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(data)))
    handler.send_header("Connection", "close")
    handler.end_headers()
    handler.wfile.write(data)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        with LOCK:
            COUNT[0] += 1
            n = COUNT[0]
        print("[mock %s] request #%d" % (MODE, n), flush=True)
        if MODE == "always500":
            respond(self, 500, json.dumps({"error": {"message": "internal boom"}}))
        elif MODE == "badjson":
            respond(self, 200, "this is definitely not json {{{")
        elif MODE == "hang":
            import time
            time.sleep(600)
        elif MODE == "failn_then_ok":
            if n <= N_FAIL:
                respond(self, 500, json.dumps({"error": {"message": "flaky upstream"}}))
                return
            with LOCK:
                i = OK_IDX[0]
                OK_IDX[0] += 1
            action = ACTIONS[i] if i < len(ACTIONS) else {"action": "message", "text": "done guard"}
            body = json.dumps({"choices": [{"message": {"content": json.dumps(action, ensure_ascii=False)}}]})
            respond(self, 200, body)

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    print("[mock %s] serving on 127.0.0.1:%d" % (MODE, PORT), flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()