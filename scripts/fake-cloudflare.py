"""假装成 Cloudflare 反爬：一律返回 403 + 挑战标记。

用来验证「撞上 Cloudflare 时自动开指纹模拟重试」这条路径——不用去找真站点。

yt-dlp 的判定（youtube 源码 generic.py）：
    403 且 (响应头 cf-mitigated == 'challenge'
            或 body 里有 <title>Attention Required! | Cloudflare</title>)
"""
import http.server
import sys

BODY = (
    b"<!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title>"
    b"</head><body>Sorry, you have been blocked</body></html>"
)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _block(self):
        self.send_response(403)
        self.send_header("Content-Type", "text/html")
        self.send_header("cf-mitigated", "challenge")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def do_GET(self):
        self._block()

    def do_HEAD(self):
        self.send_response(403)
        self.send_header("cf-mitigated", "challenge")
        self.send_header("Content-Length", "0")
        self.end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8813
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler) as httpd:
        httpd.serve_forever()
