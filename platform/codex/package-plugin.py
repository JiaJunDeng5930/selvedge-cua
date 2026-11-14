#!/usr/bin/env python3
"""Build a local Codex marketplace entry for an installed Selvedge app."""

import argparse
import json
import os
from pathlib import Path


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("marketplace", type=Path)
    options = parser.parse_args()
    executable = options.app.resolve() / "Contents/MacOS/selvedge-cua-mcp"
    if not executable.is_file() or not os.access(executable, os.X_OK):
        parser.error(f"Selvedge app executable is missing or cannot be run: {executable}")

    marketplace = options.marketplace.resolve()
    plugin = marketplace / "plugins/selvedge"
    write_json(plugin / ".codex-plugin/plugin.json", {
        "name": "selvedge",
        "version": "0.1.1",
        "description": "Observe and automate macOS app windows with Selvedge CUA MCP.",
        "mcpServers": "./.mcp.json",
        "interface": {
            "displayName": "Selvedge",
            "shortDescription": "Observe and automate macOS app windows.",
            "category": "Engineering",
            "capabilities": ["Read", "Write"],
        },
    })
    write_json(plugin / ".mcp.json", {
        "mcpServers": {
            "selvedge-cua-mcp": {"command": str(executable)},
        },
    })
    write_json(marketplace / ".agents/plugins/marketplace.json", {
        "name": "selvedge-local",
        "interface": {"displayName": "Selvedge Local"},
        "plugins": [{
            "name": "selvedge",
            "source": {"source": "local", "path": "./plugins/selvedge"},
            "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
            "category": "Engineering",
        }],
    })
    print(marketplace)


if __name__ == "__main__":
    main()
