"""Exercise the installed binary through MCP stdio.

Pass --desktop to use a real AppKit fixture in an authorized desktop session.
Pass --concurrent-capture along with --desktop to test two bundled MCP processes.
"""

import argparse
import base64
import json
from pathlib import Path
import plistlib
import select
import signal
import struct
import subprocess
import tempfile
import time


class Client:
    def __init__(self, binary):
        self.process = subprocess.Popen(
            [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0
        )
        self.next_id = 0
        self.buffer = b""

    def __enter__(self):
        self.request("initialize", {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "acceptance", "version": "1"},
        })
        self.send("notifications/initialized", {})
        return self

    def __exit__(self, error_type, *_):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            if error_type is None:
                raise
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.process.stdout.close()

    def send(self, method, params, request_id=None):
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        if request_id is not None:
            message["id"] = request_id
        self.process.stdin.write((json.dumps(message) + "\n").encode())

    def start(self, method, params):
        self.next_id += 1
        self.send(method, params, self.next_id)
        return self.next_id

    def wait(self, request_id, timeout=15):
        end = time.monotonic() + timeout
        while True:
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                response = json.loads(line)
                if response.get("id") == request_id:
                    assert "result" in response, response
                    return response["result"]
            remaining = end - time.monotonic()
            assert remaining > 0 and select.select([self.process.stdout], [], [], remaining)[0], (
                f"request {request_id} timed out"
            )
            data = self.process.stdout.read(65536)
            assert data, "MCP server closed stdout"
            self.buffer += data

    def request(self, method, params):
        return self.wait(self.start(method, params))

    def run(self, code, *, expect_error=False):
        response = self.request("tools/call", {"name": "run", "arguments": {"code": code}})
        assert response.get("isError", False) == expect_error, response["content"][0]
        return response["structuredContent"], response["content"]


def protocol(client):
    tools = client.request("tools/list", {})["tools"]
    assert [tool["name"] for tool in tools] == ["run"]
    description = tools[0]["description"]
    assert "`maxNodes` is an integer from 1 to 500" in description
    assert "`position` is `{x,y}` and `size` is `{width,height}`" in description
    assert all(f"`desktop.{method}(" in description for method in (
        "listApps", "launchApp", "readMenuBar", "selectText", "paste",
    ))
    result, _ = client.run(
        """
        const methods = [observeScreen, listWindows, readUI, pointer, keyboard, scroll, insertText, performUIAction];
        const receipts = await Promise.all([
            pointer({position: t => {throw new Error('sample at ' + t)}, durationMs: 22})
                .then(() => null, e => e.message),
            keyboard({events: [{key: 'InvalidKey', down: true}], pid: __SELF_PID__})
                .then(() => null, e => e.message)
        ]);
        return {methods: methods.map(f => typeof f), desktop: Object.keys(desktop).sort(), receipts};
        """.replace("__SELF_PID__", str(client.process.pid))
    )
    assert result["result"]["methods"] == ["function"] * 8, result
    assert result["result"]["desktop"] == ["launchApp", "listApps", "paste", "readMenuBar", "selectText"], result
    assert "sample at 0" in result["result"]["receipts"][0], result
    assert "InvalidKey" in result["result"]["receipts"][1], result
    result, _ = client.run("throw new Error('intentional failure')", expect_error=True)
    assert "intentional failure" in result["error"], result
    assert client.run("return 42;")[0]["result"] == 42
    result, _ = client.run('return await readUI({fields:["typo"]});', expect_error=True)
    assert "Unknown readUI field: typo" in result["error"], result
    result, _ = client.run('return await observeScreen({maxDimension:0});', expect_error=True)
    assert "nonzero" in result["error"].lower(), result


def result(client, code):
    return client.run(code)[0]["result"]


def png_attachments(content):
    return [item for item in content if item["type"] == "image"]


def screenshot(content, index):
    image = png_attachments(content)[index]
    assert image["mimeType"] == "image/png"
    png = base64.b64decode(image["data"])
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    return struct.unpack(">II", png[16:24])


def observation(reply, content):
    manifest = reply["observations"][0]
    viewport = manifest["viewport"]
    assert viewport["atMs"] == manifest["frames"][-1]["lastAtMs"], manifest
    width, height = screenshot(content, viewport["imageIndex"] + manifest["imageOffset"])
    rect = viewport["imageRect"]
    assert 0 < rect["width"] <= width and 0 < rect["height"] <= height, viewport
    return manifest


def local_screen_change(client, fixture_binary, pid, window_id, region, events):
    interval_ms = 90
    reply, content = client.run(f"""
        const all = node => [node,...(node.children ?? []).flatMap(all)];
        const {{window}} = await readUI({{pid:{pid}}});
        const button = all(window).find(n=>n.title==="Pulse");
        const screen = observeScreen({{
            target: {{windowId:{window_id}}}, durationMs:1450,
            intervalMs:{interval_ms}, imageBudget:3
        }});
        const before = await screen.started;
        const press = await performUIAction({{target:button.target,action:"AXPress"}});
        return {{before, press, screen:await screen}};
    """)
    pulse = events("pulse")
    pressed, = events("pulse-press")
    assert [event["visible"] for event in pulse] == [True, False], pulse
    assert pulse[1]["time"] - pulse[0]["time"] >= .4, pulse

    manifest = observation(reply, content)
    items = [(index, item) for index, image in enumerate(manifest["images"]) for item in image]
    kinds = [item["kind"] for _, item in items]
    assert all(len(item["tiles"]) == 1 for _, item in items), items
    tiles = [(index, item["tiles"][0]) for index, item in items]
    assert kinds == ["keyframe", "keyframe", "viewport"], (kinds, manifest["frames"])
    assert len(manifest["frames"]) == 3, manifest["frames"]
    assert any(len(image) > 1 for image in manifest["images"]), kinds

    pngs = png_attachments(content)
    points = []
    for (index, tile), frame in zip(tiles, manifest["frames"]):
        assert tile["desktopRect"] == region, tile
        assert (tile["firstAtMs"], tile["lastAtMs"]) == (frame["firstAtMs"], frame["lastAtMs"]), tile
        assert tile["firstAtMs"] <= tile["atMs"] <= tile["lastAtMs"], tile
        assert tile["lastAtMs"] > tile["firstAtMs"], tile
        x, y, w, h = (tile["imageRect"][key] for key in ("x", "y", "width", "height"))
        attachment = index + manifest["imageOffset"]
        width, height = screenshot(content, attachment)
        assert 0 <= x <= width - w and 0 <= y <= height - h, tile
        assert w >= region["width"] and h >= region["height"], tile
        for dx, dy in ((9, 9), (71, 9), (9, 67), (71, 67), (40, 38)):
            points.append([attachment, x + dx * w // region["width"], y + dy * h // region["height"]])

    a, b, final_a = (tile for _, tile in tiles)
    assert a["firstAtMs"] == reply["result"]["before"]["atMs"], a
    assert a["lastAtMs"] < b["firstAtMs"] < b["lastAtMs"] < final_a["firstAtMs"], tiles
    # The native press occurs between the first frame and the dispatch receipt.
    # The receipt includes IPC latency, so use those two bounds to locate the
    # fixture's transitions on the server clock.
    before_at = reply["result"]["before"]["atMs"]
    sent_at = reply["result"]["press"]["sentAtMs"]
    for earlier, later, event in zip((a, b), (b, final_a), pulse):
        elapsed = (event["time"] - pressed["time"]) * 1000
        earliest, latest = before_at + elapsed, sent_at + elapsed
        assert earlier["lastAtMs"] <= latest + interval_ms, (earlier, latest)
        assert later["firstAtMs"] >= earliest - interval_ms, (later, earliest)
    viewport = manifest["viewport"]
    index, _ = tiles[-1]
    assert viewport["imageIndex"] == index, viewport
    assert viewport["imageRect"] == final_a["imageRect"] and viewport["desktopRect"] == region, viewport
    assert viewport["atMs"] == final_a["atMs"] == final_a["lastAtMs"], viewport

    colors = json.loads(subprocess.check_output(
        [fixture_binary, "--sample-pngs"],
        input=json.dumps({"pngs": [png["data"] for png in pngs], "points": points}).encode(),
    ))
    red, green, blue = (255, 0, 0), (0, 255, 0), (0, 0, 255)
    white, black = (255, 255, 255), (0, 0, 0)
    expected = [rgb for marker in (white, black, white) for rgb in (red, green, red, blue, marker)]
    assert len(colors) == len(expected) and all(
        max(abs(channel - target) for channel, target in zip(rgb, expected_rgb)) <= 64
        for rgb, expected_rgb in zip(colors, expected)
    ), (colors, expected)
    print("desktop local change preserved three complete frames and the final viewport", flush=True)


def concurrent_capture(client, window_id, binary):
    request = {
        "name": "run",
        "arguments": {
            "code": f"""return await observeScreen({{
                target: {{windowId: {window_id}}}, durationMs: 650, intervalMs: 125
            }});""",
        },
    }
    # The first client has already captured a window and display. Both clients
    # must still receive frames when a second process registers with macOS.
    with Client(binary) as second:
        requests = [(c, c.start("tools/call", request)) for c in (client, second)]
        for c, request_id in requests:
            response = c.wait(request_id)
            assert not response.get("isError"), response["content"][0]
            manifest = observation(response["structuredContent"], response["content"])
            assert sum(frame["sampleCount"] for frame in manifest["frames"]) >= 2, manifest
    print("concurrent window capture passed for two MCP processes", flush=True)


def desktop_text(client, pid, events):
    result(client, f"""
        const ui = await readUI({{pid:{pid}}});
        return await insertText({{target:ui.focused.target,text:"Hello 世界🙂"}});
    """)
    time.sleep(.15)
    assert events("text")[-1]["value"] == "Hello 世界🙂", events("text")

    paste = result(client, f"""
        const {{focused: {{target}}}} = await readUI({{pid:{pid}}});
        const selection = await desktop.selectText({{target,text:"世界🙂",prefix:"Hello "}});
        const paste = await desktop.paste({{target,text:"background"}});
        return {{selection: selection.range,paste}};
    """)
    assert paste == {"selection": {"location": 6, "length": 4}, "paste": {"pid": pid, "clipboardRestored": True}}, paste
    assert events("text")[-1]["value"] == "Hello background", (events("paste"), events("text"))
    rich_paste = result(client, f"""
        const {{focused: {{target}}}} = await readUI({{pid:{pid}}});
        await desktop.selectText({{target,text:"background"}});
        return await desktop.paste({{target,format:"html",text:"<strong>rich</strong>",plainText:"rich"}});
    """)
    assert rich_paste == {"pid": pid, "clipboardRestored": True}, rich_paste
    assert events("paste")[-1]["html"] and events("text")[-1]["value"] == "Hello rich"


def desktop(client, fixture, fixture_binary, log_path, concurrent_binary, helpers_only=False):
    def events(kind):
        if not log_path.exists():
            return []
        return [e for e in json.loads(log_path.read_text()) if e["kind"] == kind]

    end = time.monotonic() + 8
    while not (ready_events := events("ready")) and time.monotonic() < end:
        time.sleep(.05)
    assert ready_events, "AppKit fixture did not start. Run --desktop in an active, unlocked desktop session."
    ready, = ready_events
    x, y, pid = ready["x"], ready["y"], fixture.pid
    window_id = ready["windowId"]

    def desktop_state():
        state = json.loads(subprocess.check_output([
            fixture_binary, "--desktop-state", str(pid), str(window_id),
        ]))
        assert state["frontmost"] != pid and not events("activated"), (state, events("activated"))
        assert not state["fixtureAboveForeground"], state
        assert not events("geometry"), events("geometry")
        assert state["fixtureBounds"], state
        return state

    before = desktop_state()
    assert before["frontmost"] != pid, before
    application = result(client, f"""
        const app = (await desktop.listApps()).apps.find(app => app.bundleId === "dev.selvedge.desktop-fixture");
        const launched = await desktop.launchApp({{bundleId: app.bundleId}});
        const {{menuBar}} = await desktop.readMenuBar({{pid: {pid}, maxDepth: 2, fields: ["title"]}});
        return {{pids: app.pids, launched, menuRole: menuBar.role,
                 menuItems: menuBar.children.filter(node => node.role === "AXMenuBarItem" && node.title).length}};
    """)
    assert application["pids"] == [pid], application
    assert application["launched"] == {"bundleId": "dev.selvedge.desktop-fixture", "pid": pid}, application
    assert application["menuRole"] == "AXMenuBar" and application["menuItems"] > 0, application
    after = desktop_state()
    assert after == before, ("App discovery or menu read changed the desktop state", before, after)
    if helpers_only:
        desktop_text(client, pid, events)
        after = desktop_state()
        assert after == before, ("Text selection or paste changed the desktop state", before, after)
        print("App discovery, launch reuse, menus, text selection and clipboard paste passed", flush=True)
        return
    # An exact-window capture can be the first operation of a new server.
    for target in ({"target": {"windowId": window_id}}, {}):
        reply, content = client.run(f"return await observeScreen({json.dumps(target)});")
        assert len(observation(reply, content)["frames"]) == 1
    if concurrent_binary:
        concurrent_capture(client, window_id, concurrent_binary)
    end = time.monotonic() + 5
    while True:
        ui = result(client, f"return await readUI({{pid:{pid},maxDepth:1,maxNodes:20}});")
        if (ui["window"] or {}).get("title") == "Selvedge CUA Test Fixture":
            break
        assert time.monotonic() < end, "AppKit fixture did not expose its background window"
        time.sleep(.1)
    assert ui["window"]["windowId"] == window_id, window_id
    compact = result(client, f"""
        const ui = await readUI({{pid:{pid}, fields:["title"]}});
        const all = node => [node, ...node.children.flatMap(all)];
        const nodes = all(ui.window);
        const button = nodes.find(node => node.title === "Fixture button");
        return {{
            windowId: ui.window.windowId,
            button: Boolean(button?.target),
            fields: [...new Set([...nodes, ui.focused].flatMap(Object.keys))].sort(),
        }};
    """)
    assert compact == {
        "windowId": window_id, "button": True,
        "fields": ["children", "role", "target", "title", "windowId"],
    }, compact
    print("desktop display and window capture passed", flush=True)
    local_screen_change(client, fixture_binary, pid, ready["pulseWindowId"], ready["pulseRegion"], events)
    after = desktop_state()
    assert after == before, ("Background observation changed foreground or hardware cursor", before, after)
    reply, content = client.run(f"""
        const screen = observeScreen({{
            target: {{windowId:{window_id}}}, durationMs: 900, intervalMs: 45, imageBudget: 4
        }});
        const first = await screen.started;
        const shift = await keyboard({{
            pid:{pid}, events:[{{key:'ShiftLeft', down:true}}], hold:true
        }});
        const a = await pointer({{
            position:t=>({{x:{x+70}+t/3,y:{y+135}+t*t/3600}}),
            durationMs:300, windowId:{window_id}, buttons:[0], operation:shift.operation, hold:true
        }});
        const mid = await readUI({{pid:{pid}}});
        const b = await pointer({{
            position:t=>({{x:{x+70}+(300+t)/3,y:{y+135}+(300+t)**2/3600}}),
            durationMs:300, buttons:[0], operation:a.operation
        }});
        return {{first, shift, a, mid:mid.input, b, screen:await screen}};
    """)
    draw = reply["result"]
    assert draw["first"]["atMs"] <= draw["shift"]["startedAtMs"], draw
    assert draw["mid"]["operation"] == draw["a"]["operation"], draw
    assert draw["mid"]["held"]["buttons"] == [0], draw
    assert draw["mid"]["held"]["keys"] == [{"key": "ShiftLeft", "pid": pid}], draw
    assert draw["b"]["operation"] is None
    assert draw["b"]["events"][-1]["scheduledMs"] == 300
    assert draw["b"]["releases"][0]["control"] == {"button": 0}, draw
    drawing = observation(reply, content)
    assert len(drawing["frames"]) > 2, drawing
    assert len(events("down")) == len(events("up")) == 1, log_path.read_text()
    assert desktop_state() == before, "Background drag changed foreground or hardware cursor"
    drags = events("drag")
    assert len(drags) >= 8 and all(e["shift"] for e in drags), drags
    assert len({e["event"] for e in events("down") + drags + events("up")}) == 1
    assert events("up")[0]["strokes"] == 1
    assert events("up")[0]["time"] - events("down")[0]["time"] >= .5
    assert drags[-1]["x"] - drags[0]["x"] > 150
    print(f"desktop observed one continuous stroke with {len(drags)} drag events", flush=True)
    result(client, f"""return await keyboard({{pid:{pid}, events:[
        {{key:'MetaLeft',down:true}}, {{atMs:50,key:'KeyZ',down:true}},
        {{atMs:85,key:'KeyZ',down:false}}, {{atMs:100,key:'MetaLeft',down:false}}
    ]}});""")
    time.sleep(.12)
    assert [e["strokes"] for e in events("undo")] == [0], log_path.read_text()

    desktop_text(client, pid, events)
    assert desktop_state() == before, "Text selection or paste changed foreground or hardware cursor"

    value = result(client, f"""
        const read = () => readUI({{pid:{pid}}});
        const all = node => [node,...(node.children ?? []).flatMap(all)];
        const {{window}} = await read();
        const nodes = all(window);
        const button = nodes.find(n=>n.title==="Fixture button");
        const slider = nodes.find(n=>n.identifier==="fixture-slider");
        await performUIAction({{target:button.target,action:"AXPress"}});
        await performUIAction({{target:slider.target,action:"AXSetValue",value:7}});
        return all((await read()).window).find(n=>n.identifier==="fixture-slider").value;
    """)
    assert value == 7 and events("pressed")
    reply, _ = client.run(f"""
        const ui = await readUI({{pid:{pid}}});
        const {{target}} = ui.focused;
        for(let i=0;i<8;i++) await readUI({{pid:{pid},maxDepth:1}});
        await insertText({{target,text:'stale text'}});
    """, expect_error=True)
    assert "expired" in reply["error"], reply

    reply, _ = client.run(f"""
        const pressed = await keyboard({{pid:{pid},events:[{{key:'ShiftLeft',down:true}}],hold:true}});
        throw new Error("abort after " + pressed.operation);
    """, expect_error=True)
    assert "abort after" in reply["error"], reply
    snapshot = result(client, f"return (await readUI({{pid:{pid}}})).input;")
    assert snapshot["operation"] is None and snapshot["held"] == {"buttons": [], "keys": []}, snapshot

    req = client.start("tools/call", {"name":"run", "arguments":{"code":f"""
        await keyboard({{pid:{pid},events:[
            {{key:'ShiftLeft',down:true}}, {{atMs:10000,key:'KeyA',down:true}}
        ]}});
    """}})
    time.sleep(.3)
    client.send("notifications/cancelled", {"requestId": req, "reason": "acceptance cancellation"})
    started = time.monotonic()
    snapshot = result(client, f"return (await readUI({{pid:{pid}}})).input;")
    assert time.monotonic() - started < 4, snapshot
    assert snapshot["operation"] is None and snapshot["held"] == {"buttons": [], "keys": []}, snapshot

    reply, content = client.run(f"""
        const screen=observeScreen({{target:{{windowId:{window_id}}}, durationMs:2400, intervalMs:100}});
        await screen.started;
        await scroll({{windowId:{window_id},x:{x+520},y:{y+220},events:[
            {{atMs:200,dx:18,dy:47}},{{atMs:650,dy:41}},
            {{atMs:1050,dy:38}},{{atMs:1450,dy:46}},{{atMs:1850,dy:47}}
        ]}});
        return await screen;
    """)
    scroll = observation(reply, content)
    assert len(events("scroll")) == 5 and events("scroll")[0]["dx"] and events("scroll")[0]["dy"], events("scroll")
    kinds = [item["kind"] for image in scroll["images"] for item in image]
    assert "scroll" in kinds, kinds
    after = desktop_state()
    assert after == before, ("Background run changed foreground or hardware cursor", before, after)
    print("desktop undo, text, AX, cancellation and scroll passed with foreground and cursor unchanged", flush=True)

    fixture.send_signal(signal.SIGUSR1)
    end = time.monotonic() + 5
    while True:
        secondary_events = events("secondary")
        windows = result(client, 'return await listWindows({bundleId:"dev.selvedge.desktop-fixture"});')
        visible = {w["windowId"] for w in windows["windows"] if w["pid"] == pid and w["onScreen"]}
        if secondary_events and {window_id, secondary_events[0]["windowId"]} <= visible:
            secondary_id = secondary_events[0]["windowId"]
            break
        assert time.monotonic() < end, windows
        time.sleep(.1)
    secondary = result(client, f"""
        return await readUI({{windowId:{secondary_id}, fields:["title"]}});
    """)
    assert secondary["window"]["title"] == "Secondary Fixture Window", secondary
    assert secondary["windowInput"] == {
        "status": "ready", "windowId": secondary_id,
    }, secondary
    assert secondary["focused"] is None, secondary
    missing = result(client, "return await readUI({windowId:4294967295});")
    assert missing["window"] is None
    assert missing["windowInput"] == {"status": "unavailable", "windowId": 4294967295}, missing
    codes = result(client, """
        const at = () => ({x:0,y:0});
        const caught = options => pointer(options).catch(error => error.code);
        return [await caught({position:at}), await caught({windowId:4294967295, position:at})];
    """)
    assert codes == ["windowRequired", "windowUnavailable"], codes
    reply, _ = client.run(
        "return await scroll({windowId:4294967295,x:0,y:0,events:[{dy:1}]});",
        expect_error=True,
    )
    assert reply["code"] == "windowUnavailable", reply

    def capture(options):
        reply, content = client.run(f"return await observeScreen({json.dumps(options)});")
        viewport = observation(reply, content)["viewport"]
        return viewport, screenshot(content, viewport["imageIndex"])

    overview_region = {"region": {"x": x + 12, "y": y + 70, "width": 360, "height": 450}}
    native, native_size = capture({"target": overview_region})
    overview, overview_size = capture({"target": overview_region, "maxDimension": 180})
    assert overview["desktopRect"] == native["desktopRect"], (overview, native)
    assert overview_size == (144, 180), overview_size
    assert native_size[0] > overview_size[0] and native_size[1] > overview_size[1], native_size
    after = desktop_state()
    assert after == before, ("Window discovery changed foreground or hardware cursor", before, after)
    print("desktop discovery, exact AX window, input error codes, and overview coordinates passed", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--desktop", action="store_true")
    group.add_argument("--desktop-helpers", action="store_true")
    parser.add_argument("--concurrent-capture", action="store_true")
    options = parser.parse_args()
    binary = options.binary.resolve()
    if options.concurrent_capture and not (
        options.desktop and binary.parent.name == "MacOS"
        and binary.parents[1].name == "Contents" and binary.parents[2].suffix == ".app"
    ):
        parser.error("--concurrent-capture requires --desktop and an executable in an app bundle")
    with Client(binary) as client:
        protocol(client)
        print("MCP protocol passed")
        if options.desktop or options.desktop_helpers:
            with tempfile.TemporaryDirectory(prefix="selvedge-acceptance-") as directory:
                root = Path(directory)
                source = Path(__file__).with_name("DesktopFixture.swift")
                contents = root / "Fixture.app" / "Contents"
                fixture_binary = contents / "MacOS" / "fixture"
                fixture_binary.parent.mkdir(parents=True)
                (contents / "Info.plist").write_bytes(plistlib.dumps({
                    "CFBundleIdentifier": "dev.selvedge.desktop-fixture",
                    "CFBundleExecutable": "fixture",
                    "CFBundlePackageType": "APPL",
                }))
                subprocess.run(["swiftc", source, "-o", fixture_binary], check=True)
                with (root / "fixture.log").open("wb") as log:
                    fixture = subprocess.Popen(
                        [fixture_binary, root / "events.json"], stdout=log, stderr=log
                    )
                try:
                    desktop(
                        client, fixture, fixture_binary, root / "events.json",
                        binary if options.concurrent_capture else None,
                        options.desktop_helpers,
                    )
                finally:
                    fixture.terminate()
                    fixture.wait(timeout=5)


if __name__ == "__main__":
    main()
