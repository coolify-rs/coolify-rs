"""A session on the coolify-rs instance, for what has no API route. The password is never printed."""
import http.cookiejar
import json
import os
import re
import urllib.error
import urllib.request

import databases_diff as d


class Rs:
    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.open = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.base = d.BASES["rs"]
        login = os.path.join(d.ROOT, "compat/reference/secrets/ui-login.txt")
        self.password = re.search(r"^password:\s*(.+)$", open(login).read(), re.M).group(1).strip()
        self.csrf = ""
        _, b = self.post("/ui/session", {"email": "alice@example.test", "password": self.password})
        self.csrf = json.loads(b).get("csrf_token", "")

    def post(self, path, body):
        r = urllib.request.Request(self.base + path, data=json.dumps(body).encode(), method="POST")
        r.add_header("Content-Type", "application/json")
        r.add_header("Accept", "application/json")
        r.add_header("X-CSRF-Token", self.csrf)
        try:
            with self.open.open(r) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()
