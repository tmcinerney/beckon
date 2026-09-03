#!/usr/bin/env python3
"""Minimal Beckon display plugin that records readable state snapshots."""

import json
import pathlib
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} OUTPUT", file=sys.stderr)
        return 2

    output = pathlib.Path(sys.argv[1]).expanduser()
    with output.open("a", encoding="utf-8", buffering=1) as log:
        for line in sys.stdin:
            message = json.loads(line)
            if message.get("protocol") != "beckon.display" or message.get("version") != 1:
                print("unsupported Beckon display protocol", file=sys.stderr)
                return 1
            if message.get("type") == "hello":
                log.write(f"plugin={message['plugin_id']} connected\n")
            elif message.get("type") == "render":
                states = ", ".join(
                    f"{key['key']}={key['state'] or 'unbound'}" for key in message["plan"]["keys"]
                )
                log.write(f"sequence={message['sequence']} {states}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
