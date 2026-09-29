"""假装成反爬站点：返回 403。用来验证「被挡时自动开指纹模拟重试」。

不用去找真站点——真站点不好找，也不该拿别人的站压测。

两种形态，对应现实中两种不同的报错：

  `/`          403 + `cf-mitigated: challenge` + Cloudflare 标题
               -> yt-dlp 认得出，报 `Got HTTP Error 403 caused by Cloudflare
                  anti-bot challenge; try again with --extractor-args ...`

  `/plain403`  光是 403，没有任何 Cloudflare 标记
               -> yt-dlp 只报 `Unable to download webpage: HTTP Error 403`
                  **实测用户遇到的正是这一种**（报错里根本没有 Cloudflare 字样），
                  而它以前会被应用误判成「网络不可达（检查代理设置）」。

用法：python scripts/fake-cloudflare.py 8813
"""
import http.server
import sys

CLOUDFLARE_BODY = (
    b"<!DOCTYPE html><html><head><title>Attention Required! | Cloudflare</title>"
    b"</head><body>Sorry, you have been blocked</body></html>"
)
PLAIN_BODY = b"<!DOCTYPE html><html><body>403</body></html>"


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _block(self, cloudflare_marker: bool, body: bytes):
        self.send_response(403)
        self.send_header("Content-Type", "text/html")
        if cloudflare_marker:
            self.send_header("cf-mitigated", "challenge")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.startswith("/plain403"):
            self._block(False, PLAIN_BODY)
        else:
            self._block(True, CLOUDFLARE_BODY)

    def do_HEAD(self):
        self.send_response(403)
        if not self.path.startswith("/plain403"):
            self.send_header("cf-mitigated", "challenge")
        self.send_header("Content-Length", "0")
        self.end_headers()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8813
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler) as httpd:
        httpd.serve_forever()
