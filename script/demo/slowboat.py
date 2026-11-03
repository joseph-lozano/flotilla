# A stand-in for boat's API that waits, then refuses every request with a 401, so a recording
# can show what Flotilla does while boat is thinking and after it says no.
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DELAY = float(sys.argv[2]) if len(sys.argv) > 2 else 2.5

class Refuse(BaseHTTPRequestHandler):
    def answer(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        time.sleep(DELAY)
        body = json.dumps({"ok": False, "error": {"code": "unauthorized", "message": "this API key isn't valid"}}).encode()
        self.send_response(401)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = do_POST = do_PATCH = do_DELETE = do_PUT = answer

    def log_message(self, fmt, *args):
        sys.stderr.write("[slowboat] %s %s\n" % (self.command, self.path))

ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Refuse).serve_forever()
