"""Serve the tip splitter page and its JSON endpoint."""

import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from tips import split_bill

PAGE = Path(__file__).with_name("static") / "index.html"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/":
            self.reply(200, "text/html; charset=utf-8", PAGE.read_bytes())
        elif url.path == "/api/split":
            query = parse_qs(url.query)
            try:
                share = split_bill(
                    float(query["amount"][0]),
                    float(query["tip"][0]),
                    int(query["people"][0]),
                )
            except (KeyError, ArithmeticError, ValueError) as error:
                body = {"error": str(error) or "Enter an amount, a tip and a number of people"}
                self.reply(400, "application/json", json.dumps(body).encode())
            else:
                self.reply(200, "application/json", json.dumps({"share": str(share)}).encode())
        else:
            self.reply(404, "text/plain; charset=utf-8", b"Not found")

    def reply(self, status, kind, body):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", 8000), Handler).serve_forever()
