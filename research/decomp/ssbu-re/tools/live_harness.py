#!/usr/bin/env python3
"""
R-86 — Eden live validation harness (stub).

Drives Visionary's live CSS path over the same TCP framing the editor uses,
and validates that the resident ui_chara_db changed as requested.

This is a stub that documents the harness contract so the RE work can land
without waiting for Eden.  The real harness will be filled once R-80/R-81 are
measured; until then it just shows how to talk to the plugin and what to
assert.

Usage:
  python3 live_harness.py --eden-sd /path/to/sdmc --probe
  python3 live_harness.py --eden-sd /path/to/sdmc --move mario:5 --hide ridley
"""
import argparse, json, socket, time, pathlib, sys

TCP_ADDR = "127.0.0.1:7878"  # same as rust_parameter_manager

def frame(obj):
    payload = json.dumps(obj)
    return f"<TCP_MESSAGE>{payload}</TCP_MESSAGE>".encode()

def send_one(eden_sd: pathlib.Path, order, hidden):
    # In the real harness this opens a TCP socket to the plugin and waits for
    # roster_probe acks.  Here we just show the framing and the file fallback.
    msg = {"live_css_order": order, "live_css_hidden": sorted(hidden)}
    print("would send:", frame(msg).decode()[:200])
    probe = frame({"command": "roster_probe"})
    print("would send probe:", probe.decode()[:200])
    # File fallback artifact to inspect:
    live = eden_sd / "ultimate/mods/visionary_roster_live/ui/param/database/ui_chara_db.prc"
    print("file fallback would be at:", live)
    print("check diag log at: sd:/plugin.log or Eden's log for 'live_css_order' and 'roster_probe'")
    # TODO(R-80): once heap offsets known, peek the resident buffer and diff by name_id.

def main():
    ap = argparse.ArgumentParser(description="R-86 live harness stub")
    ap.add_argument("--eden-sd", type=pathlib.Path, required=True, help="Eden SD root")
    ap.add_argument("--move", nargs="*", default=[], help="name_id:order e.g. mario:5")
    ap.add_argument("--hide", nargs="*", default=[], help="name_id to hide")
    ap.add_argument("--probe", action="store_true", help="just probe")
    args = ap.parse_args()
    order = {}
    for m in args.move:
        if ":" not in m:
            print(f"bad --move {m}, want name_id:order", file=sys.stderr)
            sys.exit(2)
        k, v = m.split(":", 1)
        order[k] = int(v)
    hidden = set(args.hide)
    if args.probe and not order and not hidden:
        print("probe only")
    send_one(args.eden_sd, order, hidden)
    print("R-86: harness stub done — wire up TCP + peek diff once heap table is ready")

if __name__ == "__main__":
    main()
