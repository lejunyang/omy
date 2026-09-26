#!/usr/bin/env python3
"""Android 外部编辑 E2E 专用的最小 WebDAV 服务器。

仅监听回环地址，支持 omy 本轮需要的 OPTIONS / PROPFIND / GET / HEAD / PUT，
并用内容哈希生成强 ETag，便于验证 If-Match 冲突保护。无第三方依赖。
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import os
from email.utils import formatdate
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import quote, unquote, urlsplit
from xml.sax.saxutils import escape


def etag(data: bytes) -> str:
    return '"' + hashlib.sha256(data).hexdigest() + '"'


class DavHandler(BaseHTTPRequestHandler):
    server_version = "omy-e2e-webdav/1"

    def log_message(self, fmt: str, *args: object) -> None:
        print(f"{self.command} {self.path} - {fmt % args}", flush=True)

    @property
    def root(self) -> Path:
        return self.server.root  # type: ignore[attr-defined]

    def authorized(self) -> bool:
        expected = self.server.authorization  # type: ignore[attr-defined]
        if not expected:
            return True
        if self.headers.get("Authorization") == expected:
            return True
        self.send_response(401)
        self.send_header("WWW-Authenticate", 'Basic realm="omy-e2e"')
        self.send_header("Content-Length", "0")
        self.end_headers()
        return False

    def local_path(self) -> Path | None:
        rel = unquote(urlsplit(self.path).path).lstrip("/")
        candidate = (self.root / rel).resolve()
        try:
            candidate.relative_to(self.root.resolve())
        except ValueError:
            return None
        return candidate

    def do_OPTIONS(self) -> None:
        if not self.authorized():
            return
        self.send_response(200)
        self.send_header("DAV", "1")
        self.send_header("Allow", "OPTIONS, PROPFIND, GET, HEAD, PUT")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_PROPFIND(self) -> None:
        if not self.authorized():
            return
        path = self.local_path()
        if path is None or not path.exists():
            self.send_error(404)
            return
        items = [path]
        if path.is_dir() and self.headers.get("Depth", "1") != "0":
            items.extend(sorted(path.iterdir()))
        responses = []
        for item in items:
            rel = item.relative_to(self.root).as_posix()
            href = "/" + quote(rel, safe="/")
            if item.is_dir() and not href.endswith("/"):
                href += "/"
            stat = item.stat()
            resource = "<D:collection/>" if item.is_dir() else ""
            body = (
                "<D:response><D:href>" + escape(href) + "</D:href>"
                "<D:propstat><D:prop>"
                f"<D:displayname>{escape(item.name or '/')}</D:displayname>"
                f"<D:resourcetype>{resource}</D:resourcetype>"
                f"<D:getcontentlength>{0 if item.is_dir() else stat.st_size}</D:getcontentlength>"
                f"<D:getlastmodified>{formatdate(stat.st_mtime, usegmt=True)}</D:getlastmodified>"
                + ("" if item.is_dir() else f"<D:getetag>{etag(item.read_bytes())}</D:getetag>")
                + "</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
            )
            responses.append(body)
        payload = ("<?xml version=\"1.0\" encoding=\"utf-8\"?>"
                   "<D:multistatus xmlns:D=\"DAV:\">" + "".join(responses) +
                   "</D:multistatus>").encode()
        self.send_response(207)
        self.send_header("Content-Type", "application/xml; charset=utf-8")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def send_file(self, include_body: bool) -> None:
        if not self.authorized():
            return
        path = self.local_path()
        if path is None or not path.is_file():
            self.send_error(404)
            return
        data = path.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("ETag", etag(data))
        self.send_header("Last-Modified", formatdate(path.stat().st_mtime, usegmt=True))
        self.end_headers()
        if include_body:
            self.wfile.write(data)

    def do_GET(self) -> None:
        self.send_file(True)

    def do_HEAD(self) -> None:
        self.send_file(False)

    def do_PUT(self) -> None:
        if not self.authorized():
            return
        path = self.local_path()
        if path is None:
            self.send_error(400)
            return
        current = path.read_bytes() if path.is_file() else None
        match = self.headers.get("If-Match")
        if match is not None and (current is None or match != etag(current)):
            self.send_response(412)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length", "0"))
        data = self.rfile.read(length)
        path.parent.mkdir(parents=True, exist_ok=True)
        temp = path.with_name(path.name + ".tmp")
        temp.write_bytes(data)
        os.replace(temp, path)
        self.send_response(204)
        self.send_header("ETag", etag(data))
        self.send_header("Content-Length", "0")
        self.end_headers()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("root")
    parser.add_argument("--port", type=int, default=8799)
    parser.add_argument("--user", default="omy")
    parser.add_argument("--password", default="test")
    args = parser.parse_args()
    root = Path(args.root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), DavHandler)
    server.root = root  # type: ignore[attr-defined]
    raw = f"{args.user}:{args.password}".encode()
    server.authorization = "Basic " + base64.b64encode(raw).decode()  # type: ignore[attr-defined]
    print(f"WebDAV {root} -> http://127.0.0.1:{args.port}/", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
