#!/usr/bin/env python3
"""AIR-399 offline serve-smoke checker.

Speaks just enough MCP streamable HTTP to call the `search` tool on a locally booted
apollo-mcp-server, replays every query in the checked-in search baseline
(crates/apollo-mcp-server/src/introspection/tools/testdata/search_baseline/baseline.json),
and asserts the returned type definitions reproduce the fixture's `result_types`.

Prints one "PASS <query-id>" / "FAIL <query-id> ..." line per baseline query and a final
"RESULT: PASS|FAIL" line; exits non-zero on any failure.

Stdlib only — no pip dependencies.
"""

import argparse
import json
import re
import sys
import urllib.request

PROTOCOL_VERSION = "2025-06-18"
TYPE_DEF_RE = re.compile(
    r"^\s*(?:extend\s+)?(?:type|interface|enum|union|input|scalar)\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def parse_body(raw, content_type):
    """Return the list of JSON-RPC messages in a response body (JSON or SSE)."""
    text = raw.decode("utf-8", "replace")
    if "text/event-stream" in (content_type or ""):
        messages = []
        for line in text.splitlines():
            if line.startswith("data:"):
                data = line[len("data:"):].strip()
                if data:
                    messages.append(json.loads(data))
        return messages
    text = text.strip()
    return [json.loads(text)] if text else []


class McpClient:
    def __init__(self, endpoint):
        self.endpoint = endpoint
        self.session_id = None
        self.next_id = 1

    def _post(self, payload):
        headers = {
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
        }
        if self.session_id:
            headers["mcp-session-id"] = self.session_id
            headers["mcp-protocol-version"] = PROTOCOL_VERSION
        req = urllib.request.Request(
            self.endpoint, data=json.dumps(payload).encode(), headers=headers, method="POST"
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            sid = resp.headers.get("mcp-session-id")
            if sid:
                self.session_id = sid
            return parse_body(resp.read(), resp.headers.get("content-type"))

    def request(self, method, params):
        req_id = self.next_id
        self.next_id += 1
        messages = self._post(
            {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}
        )
        for message in messages:
            if message.get("id") == req_id:
                if "error" in message:
                    raise RuntimeError(f"{method} failed: {message['error']}")
                return message.get("result", {})
        raise RuntimeError(f"no response to {method} (got: {messages!r})")

    def initialize(self):
        result = self.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "air399-search-baseline-smoke", "version": "0.0.1"},
            },
        )
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return result

    def call_search(self, terms):
        return self.request("tools/call", {"name": "search", "arguments": {"terms": terms}})


def type_names(content_blocks):
    """Extract type definition names from the search tool's SDL content blocks."""
    names = set()
    for block in content_blocks:
        if block.get("type") != "text":
            continue
        in_description = False
        for line in block.get("text", "").splitlines():
            # Skip block description strings so their prose can't match the regex.
            if line.strip().startswith('"""'):
                if not (in_description is False and line.strip().count('"""') == 2):
                    in_description = not in_description
                continue
            if in_description:
                continue
            match = TYPE_DEF_RE.match(line)
            if match:
                names.add(match.group(1))
    return sorted(names)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", required=True, help="MCP endpoint, e.g. http://127.0.0.1:4599/mcp")
    parser.add_argument("--baseline", required=True, help="path to baseline.json")
    args = parser.parse_args()

    with open(args.baseline, encoding="utf-8") as f:
        baseline = json.load(f)

    client = McpClient(args.endpoint)
    client.initialize()

    failures = 0
    for query in baseline["queries"]:
        expected = sorted(query["result_types"])
        try:
            result = client.call_search(query["terms"])
            actual = type_names(result.get("content", []))
        except Exception as e:  # noqa: BLE001 - smoke check: report and count any failure
            print(f"FAIL {query['id']}: request error: {e}")
            failures += 1
            continue
        if actual == expected:
            print(f"PASS {query['id']}")
        else:
            missing = sorted(set(expected) - set(actual))
            unexpected = sorted(set(actual) - set(expected))
            print(
                f"FAIL {query['id']}: terms={query['terms']} "
                f"missing={missing} unexpected={unexpected}"
            )
            failures += 1

    total = len(baseline["queries"])
    print(f"{total - failures}/{total} baseline queries reproduced")
    print("RESULT: PASS" if failures == 0 else "RESULT: FAIL")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
