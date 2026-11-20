"""Exercise Locked Use through MCP stdio with a fake helper.

The helper simulates all lock and unlock transitions.
"""

import argparse
from pathlib import Path
import tempfile

from acceptance import Client


# Every UI probe is read-only and targets an intentionally invalid PID.
READ_UI = "readUI({pid: 2147483647})"


def executable(path, source):
    path.write_text("#!/usr/bin/env python3\n" + source)
    path.chmod(0o700)
    return path


def start_client(binary, root, name, *, begin, check, finish):
    helper_dir = root / name
    helper_dir.mkdir()
    log = helper_dir / "commands.txt"
    responses = {"begin": begin, "check": check, "finish": finish}
    helper = executable(
        helper_dir / "helper",
        f"""import json
from pathlib import Path
import sys

assert sys.argv[1:] == ["--selvedge-locked-use-bridge"], sys.argv
responses = {responses!r}
with Path({str(log)!r}).open("a", buffering=1) as log:
    print("started", file=log)
    for line in sys.stdin:
        command = line.rstrip("\\n")
        print(command, file=log)
        print(json.dumps(responses[command]), flush=True)
""",
    )
    command = [str(binary), "--locked-use-helper", str(helper)]
    launcher = executable(
        helper_dir / "mcp",
        f"import os\nos.execv({command[0]!r}, {command!r})\n",
    )
    return Client(launcher), log


def commands(log):
    if not log.exists():
        return []
    return log.read_text().splitlines()


def assert_lifecycle(observed, *, min_checks):
    checks = len(observed) - 3
    assert checks >= min_checks, observed
    assert observed == ["started", "begin", *(["check"] * checks), "finish"], observed


def successful_invocation(binary, root, state, finish):
    client, log = start_client(
        binary, root, state,
        begin={"state": state}, check={"state": "ok"}, finish={"state": finish},
    )
    with client:
        result, _ = client.run("return 42;")
        assert result["result"] == 42, result
        assert not commands(log)

        result, _ = client.run(f"""
            const read = () => {READ_UI}.then(() => "read", () => "rejected");
            return await Promise.all([read(), read(), read()]);
        """)
        assert result["result"] == ["rejected"] * 3, result
        completed = commands(log)
        assert_lifecycle(completed, min_checks=3)
        if state == "protected":
            result, _ = client.run(
                f'await {READ_UI}.catch(() => null); throw new Error("script failed");',
                expect_error=True,
            )
            assert "script failed" in result["error"], result
            assert_lifecycle(commands(log)[len(completed):], min_checks=1)


def denied_invocation(binary, root, *, denied_at, before_cleanup):
    reason = f"Locked Use denied by fake helper at {denied_at}"
    begin = {"error": reason} if denied_at == "begin" else {"state": "protected"}
    check = {"error": reason} if denied_at == "check" else {"state": "ok"}
    client, log = start_client(
        binary, root, denied_at, begin=begin, check=check, finish={"state": "locked"},
    )
    with client:
        result, _ = client.run(
            f'await {READ_UI}.catch(() => null); return "caught";',
            expect_error=True,
        )
        assert "locked use" in result["error"].lower(), result
        observed = commands(log)
        assert observed == ["started", *before_cleanup, "finish"], observed


def finish_failure(binary, root, *, relocked):
    name = "physical-takeover" if relocked else "unconfirmed-lock"
    reason = "fake helper detected physical input" if relocked else "fake helper could not confirm the lock"
    finish = {"state": "locked", "error": reason} if relocked else {"error": reason}
    client, log = start_client(
        binary, root, name, begin={"state": "protected"},
        check={"state": "ok"}, finish=finish,
    )
    with client:
        code = f'await {READ_UI}.catch(() => null); return "otherwise successful";'
        result, _ = client.run(code, expect_error=True)
        assert reason in result["error"], result
        previous = commands(log)
        assert_lifecycle(previous, min_checks=1)

        client.run(code, expect_error=True)
        current = commands(log)
        if relocked:
            assert_lifecycle(current[len(previous):], min_checks=1)
        else:
            assert current == previous, current
        result, _ = client.run("return 42;")
        assert result["result"] == 42, result
        assert commands(log) == current, commands(log)


def rejects_physical_capture(binary, root):
    client, log = start_client(
        binary, root, "physical-capture", begin={"state": "protected"},
        check={"state": "ok"}, finish={"state": "locked"},
    )
    with client:
        result, content = client.run("""
            const targets = [
                {displayId: 4294967295},
                {region: {x: -100000000, y: -100000000, width: 1, height: 1}},
            ];
            const probe = target => observeScreen({target}).then(
                () => "captured", error => error.message
            );
            return await Promise.all(targets.map(probe));
        """)
        reasons = result["result"]
        assert len(reasons) == 2, reasons
        assert all("locked use" in reason.lower() for reason in reasons), reasons
        assert all(item["type"] != "image" for item in content)
        assert_lifecycle(commands(log), min_checks=0)


def rejects_app_audio(binary, root):
    client, log = start_client(
        binary, root, "app-audio", begin={"state": "protected"},
        check={"state": "ok"}, finish={"state": "locked"},
    )
    with client:
        for code in ("return await startListening({pid:1});", "return await readTranscript();"):
            result, _ = client.run(code, expect_error=True)
            assert "unavailable with Locked Use enabled" in result["error"], result
        assert commands(log) == [], commands(log)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    binary = parser.parse_args().binary.resolve()
    with tempfile.TemporaryDirectory(prefix="selvedge-locked-use-") as directory:
        root = Path(directory)
        successful_invocation(binary, root, "ordinary", "ok")
        successful_invocation(binary, root, "protected", "locked")
        denied_invocation(
            binary, root, denied_at="begin", before_cleanup=["begin"],
        )
        denied_invocation(
            binary, root, denied_at="check", before_cleanup=["begin", "check"],
        )
        finish_failure(binary, root, relocked=False)
        finish_failure(binary, root, relocked=True)
        rejects_physical_capture(binary, root)
        rejects_app_audio(binary, root)
    print("Locked Use MCP protocol and authorization passed")


if __name__ == "__main__":
    main()
