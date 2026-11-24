#!/usr/bin/env python3
"""Minimal ACP stdio agent used by the ignored Rust settings-probe test."""

import json
import sys


def main() -> int:
    for line in sys.stdin:
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            return 2
        if request.get("method") != "initialize":
            continue
        response = {
            "jsonrpc": "2.0",
            "id": request.get("id"),
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": True,
                    "promptCapabilities": {
                        "image": False,
                        "audio": False,
                        "embeddedContext": True,
                    },
                },
                "agentInfo": {
                    "name": "kite-acp-probe-fixture",
                    "title": "Kite ACP Probe Fixture",
                    "version": "1.0.0",
                },
                "authMethods": [],
            },
        }
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()
        return 0
    return 3


if __name__ == "__main__":
    raise SystemExit(main())
