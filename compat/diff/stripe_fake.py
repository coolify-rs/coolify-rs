#!/usr/bin/env python3
"""A local stand-in for the Stripe API, for the Coolify Cloud path of coolify-rs.

    python3 compat/diff/stripe_fake.py 18192

Answers the endpoints `crates/coolify-api/src/ui_subscription.rs` calls, keeps its state in memory,
and lists every request at GET /__log. Nothing here talks to Stripe.
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qsl, urlparse

STATE = {"customers": {}, "subs": {}, "sessions": {}, "log": [], "next": 1, "unpaid_invoice": False}


def next_id(prefix):
    STATE["next"] += 1
    return f"{prefix}_{STATE['next']}"


def subscription(customer, team_id, price, quantity=2):
    sid = next_id("sub")
    STATE["subs"][sid] = {
        "id": sid,
        "customer": customer,
        "status": "active",
        "metadata": {"team_id": str(team_id)},
        "cancel_at_period_end": False,
        "start_date": int(time.time()) - 86400 * 5,
        "current_period_end": 1893456000,
        "items": {"data": [{"id": "si_1", "quantity": quantity, "price": {"id": price, "currency": "usd", "lookup_key": "dynamic-monthly", "recurring": {"interval": "month"}}}]},
    }
    return STATE["subs"][sid]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def answer(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def handle_any(self):
        url = urlparse(self.path)
        path = url.path
        length = int(self.headers.get("Content-Length") or 0)
        params = dict(parse_qsl(self.rfile.read(length).decode())) if length else {}
        params.update(parse_qsl(url.query))
        if path == "/__log":
            return self.answer(200, STATE["log"])
        if path == "/__subscribe":
            return self.answer(200, subscription(params["customer"], params["team_id"], params["price"]))
        if path == "/__unpaid":
            STATE["unpaid_invoice"] = params.get("value") == "1"
            return self.answer(200, {})
        STATE["log"].append({"method": self.command, "path": path, "params": params, "auth": self.headers.get("Authorization"), "idempotency": self.headers.get("Idempotency-Key")})
        if not (self.headers.get("Authorization") or "").startswith("Bearer "):
            return self.answer(401, {"error": {"message": "No API key"}})
        method = self.command
        parts = [p for p in path.split("/") if p][1:]
        if parts == ["customers"] and method == "POST":
            cid = next_id("cus")
            STATE["customers"][cid] = {"id": cid, "email": params.get("email")}
            return self.answer(200, STATE["customers"][cid])
        if parts[:1] == ["customers"] and len(parts) == 2:
            return self.answer(200, {"id": parts[1]})
        if parts == ["subscriptions"]:
            return self.answer(200, {"data": [s for s in STATE["subs"].values() if s["customer"] == params.get("customer")]})
        if parts[:1] == ["subscriptions"] and len(parts) == 2:
            sub = STATE["subs"].get(parts[1])
            if not sub:
                return self.answer(404, {"error": {"message": "No such subscription"}})
            if method == "DELETE":
                sub["status"] = "canceled"
                del STATE["subs"][parts[1]]
                return self.answer(200, sub)
            if method == "POST":
                if "cancel_at_period_end" in params:
                    sub["cancel_at_period_end"] = params["cancel_at_period_end"] == "true"
                if "items[0][quantity]" in params:
                    sub["items"]["data"][0]["quantity"] = int(params["items[0][quantity]"])
                out = dict(sub)
                if "expand[0]" in params:
                    out["latest_invoice"] = {"id": "in_latest", "status": "open" if STATE["unpaid_invoice"] else "paid"}
                return self.answer(200, out)
            return self.answer(200, sub)
        if parts == ["checkout", "sessions"]:
            if method == "GET":
                return self.answer(200, {"data": [s for s in STATE["sessions"].values() if s["customer"] == params.get("customer") and s["status"] == "open"]})
            sid = next_id("cs")
            STATE["sessions"][sid] = {"id": sid, "mode": "subscription", "status": "open", "customer": params.get("customer"), "price": params.get("line_items[0][price]"), "url": f"http://127.0.0.1:1/checkout/{sid}", "params": params}
            return self.answer(200, STATE["sessions"][sid])
        if parts[:2] == ["checkout", "sessions"] and parts[-1] == "line_items":
            return self.answer(200, {"data": [{"price": {"id": STATE["sessions"][parts[2]]["price"]}}]})
        if parts[:2] == ["checkout", "sessions"] and parts[-1] == "expire":
            STATE["sessions"][parts[2]]["status"] = "expired"
            return self.answer(200, STATE["sessions"][parts[2]])
        if parts == ["billing_portal", "sessions"]:
            return self.answer(200, {"url": "http://127.0.0.1:1/portal"})
        if parts == ["invoices", "upcoming"]:
            quantity = int(params.get("subscription_items[0][quantity]", 2))
            return self.answer(200, {"total_tax_amounts": [{"tax_rate": "txr_1"}], "tax": 100, "subtotal": 1000, "amount_due": 300 * quantity + 700, "lines": {"data": [{"proration": True, "amount": 700}, {"proration": False, "amount": 300 * quantity}]}})
        if parts[:1] == ["tax_rates"]:
            return self.answer(200, {"percentage": 10.0, "display_name": "VAT", "jurisdiction": "EU"})
        if parts == ["invoices"]:
            return self.answer(200, {"data": [{"id": "in_1", "payment_intent": "pi_1"}]})
        if parts == ["refunds"]:
            return self.answer(200, {"id": "re_1"})
        if parts[:1] == ["invoices"] and parts[-1] == "void":
            return self.answer(200, {"id": parts[1], "status": "void"})
        return self.answer(404, {"error": {"message": f"Unknown path {path}"}})

    do_GET = do_POST = do_DELETE = handle_any


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 18192), Handler).serve_forever()
