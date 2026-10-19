"""Cart API for the victim shop: POST /cart/total with a JSON list of items."""

import json
from http.server import BaseHTTPRequestHandler, HTTPServer


def total(items):
    return sum(i["price"] * i["quantity"] for i in items)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/cart/total":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        items = json.loads(self.rfile.read(length) or b"[]")
        body = json.dumps({"total": total(items)}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 8080), Handler).serve_forever()
