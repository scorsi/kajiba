"""Smart HTTP source that holds Fetch after receiving its real want request."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import time


class Source(BaseHTTPRequestHandler):
    def reply(self, body, content_type):
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass  # The timeout/crash scenarios deliberately kill the client.

    def do_GET(self):
        assert self.path.endswith("/info/refs?service=git-upload-pack"), self.path
        advertised = subprocess.check_output(
            ["git", "upload-pack", "--stateless-rpc", "--advertise-refs", str(repo)]
        )
        self.reply(
            b"001e# service=git-upload-pack\n0000" + advertised,
            "application/x-git-upload-pack-advertisement",
        )

    def do_POST(self):
        assert self.path.endswith("/git-upload-pack"), self.path
        body = self.rfile.read(int(self.headers["Content-Length"]))
        assert b"want " in body, body
        scenario = self.path.split("/")[1]
        (control / f"{scenario}.entered").touch()
        while not (control / f"{scenario}.release").exists():
            time.sleep(0.01)
        result = subprocess.check_output(
            ["git", "upload-pack", "--stateless-rpc", str(repo)], input=body
        )
        self.reply(result, "application/x-git-upload-pack-result")

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    repo, control = Path(sys.argv[1]), Path(sys.argv[2])
    ThreadingHTTPServer(("127.0.0.1", 9425), Source).serve_forever()
