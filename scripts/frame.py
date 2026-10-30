#!/usr/bin/env python3 -I
"""Draws what Rex is showing as an HTML page, from Rex's own data: the sessions and windows
in the sidebar, each window's program status, and each block's screen as Rex formats it
(`com.superlogical.terminal.format`, html). Use it to check t3rex's panes when the Mac's
screen is locked and the app draws nothing.

    scripts/frame.py "T3 Code" "Window label prefix" --theme dark|light [--size 120x40] > frame.html
"""
import argparse
import html
import json
import subprocess
import os

REX = os.environ.get("T3REX_REX", "/Applications/Rex Beta.app/Contents/Helpers/Rex Server.rex/Contents/MacOS/rex")

THEMES = {
    # Close to Rex's default dark theme in the app, and Ghostty's default palette.
    "dark": {
        "bg": "#16181b", "side": "#111316", "fg": "#e6e6e6", "muted": "#8b8e94", "sel": "#2a2d32", "line": "#24272b",
        "palette": ["#1d1f21", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6",
                    "#666666", "#d54e53", "#b9ca4a", "#e7c547", "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea"],
    },
    "light": {
        "bg": "#fbfbfa", "side": "#efefed", "fg": "#1f2328", "muted": "#7b7f86", "sel": "#ffffff", "line": "#dedede",
        "palette": ["#24292f", "#cf222e", "#116329", "#9a6700", "#0969da", "#8250df", "#1b7c83", "#6e7781",
                    "#57606a", "#a40e26", "#1a7f37", "#633c01", "#218bff", "#a475f9", "#3192aa", "#8c959f"],
    },
}


def rex(*args, data=None):
    out = subprocess.run([REX, *args], capture_output=True, text=True, input=data, timeout=20)
    if out.returncode != 0:
        raise SystemExit(out.stderr)
    return json.loads(out.stdout) if out.stdout.strip() else None


def block_call(session, block, method, args=None):
    body = {"session_id": session, "block_id": block, "method": method}
    if args is not None:
        body["args"] = args
    return rex("api", "call", "block.method", json.dumps(body))


def status_of(session, window):
    blocks = [b for layer in window["layers"] for b in layer["blocks"]]
    for b in blocks:
        try:
            recs = (block_call(session, b["block_id"], "program_status") or {}).get("records") or []
        except SystemExit:
            continue
        if recs:
            return recs[0]
    return None


def marker(status):
    if not status:
        return ""
    state = status.get("state")
    if state == "working":
        return '<span class="dots">•••</span>'
    if state in ("blocked", "done", "error"):
        return '<span class="dot"></span>'
    return ""


def leaves(node, out):
    if "block_id" in node:
        out.append(node)
    if isinstance(node.get("split"), dict):
        leaves(node["split"], out)
    for key in ("before", "after", "children"):
        child = node.get(key)
        if isinstance(child, dict):
            leaves(child, out)
        elif isinstance(child, list):
            for c in child:
                leaves(c, out)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("session")
    ap.add_argument("window")
    ap.add_argument("--theme", default="dark")
    ap.add_argument("--size", default="")
    ap.add_argument("--demo", action="store_true", help="a made-up sidebar of a busy day around the real window")
    args = ap.parse_args()
    t = THEMES[args.theme]
    sessions = rex("ls", "--json")["sessions"]
    side = []
    chosen = None
    for s in sessions:
        view = rex("session", "inspect", s["session_id"], "--json")
        side.append(f'<div class="sess">{html.escape(s["label"])}</div>')
        for w in view["windows"]:
            focused = s["label"] == args.session and w["label"].startswith(args.window) and chosen is None
            if focused:
                chosen = (s["session_id"], w)
            label = w["label"]
            if label.startswith("Window ") and w.get("focused_block_id"):
                label = "zsh"
            st = status_of(s["session_id"], w) if s["label"] == args.session else None
            side.append(
                f'<div class="win{" on" if focused else ""}"><span class="ico">›_</span>'
                f'<span class="lb">{html.escape(label)}</span>{marker(st)}</div>'
            )
    if chosen is None:
        raise SystemExit("no such window")
    if args.demo:
        # Made up, to show a busier day than this Mac has: T3's order, Rex's markers.
        fake = [
            (chosen[1]["label"], "dot", True),
            ("Which auth lib for mobile", "dot", False),
            ("Fix flaky checkout e2e test", "dot", False),
            ("Release notes 0.1.7", "", False),
            ("Bump Effect to beta.74", "", False),
            ("Migrate settings to Supabase", "dots", False),
            ("Audit prod release", "dots", False),
            ("Index open PRs by readiness", "dots", False),
        ]
        side = ['<div class="sess">T3 Code</div>']
        for label, mark, on in fake:
            m = {"dot": '<span class="dot"></span>', "dots": '<span class="dots">•••</span>'}.get(mark, "")
            side.append(f'<div class="win{" on" if on else ""}"><span class="ico">›_</span><span class="lb">{html.escape(label)}</span>{m}</div>')
        side += ['<div class="sess">dusty-creek</div>', '<div class="win"><span class="ico">›_</span><span class="lb">zsh ~/Developer/macstack</span></div>']
    session_id, window = chosen
    layer = window["layers"][0]
    panes = []
    for leaf in leaves(layer["layout"], []):
        block = leaf["block_id"]
        x0, y0, w, h = leaf["rect"]
        title = (block_call(session_id, block, "title") or {}).get("title") or "shell"
        screen = (block_call(session_id, block, "format", {"format": "html"}) or {}).get("content", "")
        rows = (block_call(session_id, block, "size") or {}).get("rows", 40)
        # Rex shows the bottom of the scrollback, so keep the last screenful.
        head, sep, body = screen.partition(">")
        lines = body.split("\n")
        screen = head + sep + "\n".join(lines[-rows:])
        panes.append(
            f'<div class="pane" style="left:{x0*100}%;top:{y0*100}%;width:calc({w*100}% - 6px);height:calc({h*100}% - 6px)">'
            f'<div class="ph"><span class="ico">›_</span> {html.escape(title)}</div>'
            f'<div class="term">{screen}</div></div>'
        )
    palette = "".join(f"--vt-palette-{i}:{c};" for i, c in enumerate(t["palette"]))
    print(f"""<!doctype html><html><head><meta charset="utf-8"><style>
:root{{{palette}}}
*{{box-sizing:border-box}}
body{{margin:0;font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text",system-ui,sans-serif}}
.app{{display:grid;grid-template-columns:230px 1fr;height:760px;background:{t['bg']};color:{t['fg']};border-radius:12px;overflow:hidden;border:1px solid {t['line']}}}
.side{{background:{t['side']};padding:44px 8px 8px;font-size:13px;display:flex;flex-direction:column;gap:2px;position:relative}}
.side::before{{content:"";position:absolute;left:16px;top:16px;width:52px;height:12px;background:radial-gradient(circle at 6px 6px,#ff5f57 5px,transparent 6px),radial-gradient(circle at 26px 6px,#febc2e 5px,transparent 6px),radial-gradient(circle at 46px 6px,#28c840 5px,transparent 6px)}}
.sess{{color:{t['muted']};font-size:11.5px;font-weight:600;padding:10px 8px 4px}}
.win{{display:flex;align-items:center;gap:8px;padding:6px 8px;border-radius:9px;white-space:nowrap;overflow:hidden}}
.win.on{{background:{t['sel']}}}
.win .lb{{overflow:hidden;text-overflow:ellipsis;flex:1}}
.ico{{font-family:ui-monospace,Menlo,monospace;font-size:10px;color:{t['muted']};border:1px solid {t['line']};border-radius:4px;padding:0 3px;position:relative}}
.dot{{width:7px;height:7px;border-radius:50%;background:#3b82f6;flex:none}}
.dots{{color:{t['muted']};letter-spacing:1px;font-size:11px}}
.main{{position:relative;margin:8px 8px 8px 0}}
.pane{{position:absolute;padding:3px;}}
.pane > .ph{{font-size:13px;font-weight:500;padding:6px 10px;color:{t['fg']}}}
.pane > .term{{font-family:"SF Mono",ui-monospace,Menlo,monospace;font-size:11.5px;line-height:1.3;padding:0 10px;height:calc(100% - 32px);overflow:hidden}}
.pane{{border:1px solid {t['line']};border-radius:10px;background:{t['bg']}}}
a{{color:inherit}}
</style></head><body><div class="app"><div class="side">{''.join(side)}</div><div class="main">{''.join(panes)}</div></div></body></html>""")


main()
