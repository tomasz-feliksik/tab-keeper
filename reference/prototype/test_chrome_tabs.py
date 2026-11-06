"""Tests for scripts/chrome_tabs.py, and that the packed extension matches its source."""

from __future__ import annotations

import io
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile
from collections.abc import Iterator
from pathlib import Path

import pytest

from scripts.chrome_tabs import (
    ChromeTabsError,
    Tab,
    ask_instance,
    discard_candidates,
    host_is_excepted,
    parse_duration_ms,
    profile_name_for_instance,
    read_discard_exceptions,
    read_frame,
    write_frame,
)

EXTENSION = Path("features/applications/chrome-tabs-extension")
HOUR_MS = 3_600_000
NOW_MS = 100 * HOUR_MS


def make_tab(**overrides: object) -> Tab:
    fields: dict[str, object] = {
        "id": 1,
        "title": "t",
        "url": "https://example.com/",
        "active": False,
        "pinned": False,
        "audible": False,
        "discarded": False,
        "last_accessed_ms": NOW_MS - 3 * HOUR_MS,
    }
    return Tab(**{**fields, **overrides})  # pyright: ignore[reportArgumentType]


def candidates(tab: Tab, exceptions: list[str] | None = None) -> list[Tab]:
    return discard_candidates(
        (tab,), idle_ms=2 * HOUR_MS, now_ms=NOW_MS, exceptions=exceptions or []
    )


def test_idle_tab_is_a_candidate() -> None:
    assert candidates(make_tab()) == [make_tab()]


def test_recently_used_tab_is_kept() -> None:
    assert candidates(make_tab(last_accessed_ms=NOW_MS - HOUR_MS)) == []


@pytest.mark.parametrize(
    "overrides",
    [
        {"active": True},
        {"pinned": True},
        {"audible": True},
        {"discarded": True},
        {"last_accessed_ms": None},
    ],
)
def test_protected_tabs_are_kept(overrides: dict[str, object]) -> None:
    assert candidates(make_tab(**overrides)) == []


def test_excepted_host_is_kept_including_subdomains() -> None:
    assert (
        candidates(make_tab(url="https://meet.google.com/abc"), ["meet.google.com"])
        == []
    )
    assert (
        candidates(make_tab(url="https://eu.teams.live.com/x"), ["teams.live.com"])
        == []
    )


def test_lookalike_host_is_not_excepted() -> None:
    assert not host_is_excepted("https://notmeet.google.com/", ["meet.google.com"])
    assert not host_is_excepted("https://evil.com/meet.google.com", ["meet.google.com"])


def test_parses_durations_and_refuses_garbage() -> None:
    assert parse_duration_ms("90m") == 90 * 60_000
    with pytest.raises(ChromeTabsError, match="bad duration"):
        parse_duration_ms("2 hours")


def test_refuses_to_run_without_the_exception_list(tmp_path: Path) -> None:
    policy = tmp_path / "policy.json"
    with pytest.raises(ChromeTabsError, match="TabDiscardingExceptions"):
        read_discard_exceptions(policy)
    policy.write_text("{}")
    with pytest.raises(ChromeTabsError, match="TabDiscardingExceptions"):
        read_discard_exceptions(policy)
    policy.write_text('{"TabDiscardingExceptions": ["meet.google.com"]}')
    assert read_discard_exceptions(policy) == ["meet.google.com"]


def test_finds_the_profile_by_instance_when_two_profiles_share_everything_else(
    tmp_path: Path,
) -> None:
    for profile, instance in (("agent", "a" * 32), ("private", "b" * 32)):
        storage = tmp_path / profile / "Local Extension Settings" / "extid"
        storage.mkdir(parents=True)
        (storage / "000003.log").write_bytes(b"\x00instance\x00" + instance.encode())
    assert profile_name_for_instance("b" * 32, "extid", tmp_path) == "private"
    assert profile_name_for_instance("c" * 32, "extid", tmp_path) is None


def test_frames_round_trip_and_end_cleanly() -> None:
    stream = io.BytesIO()
    write_frame(stream, {"a": 1})
    stream.seek(0)
    assert read_frame(stream) == {"a": 1}
    assert read_frame(stream) is None


@pytest.fixture
def short_tmp() -> Iterator[Path]:
    """A directory short enough for AF_UNIX, whose paths stop at 107 bytes."""
    with tempfile.TemporaryDirectory(dir="/tmp", prefix="ct") as directory:
        yield Path(directory)


def test_host_relays_a_request_to_the_extension_and_back(short_tmp: Path) -> None:
    tmp_path = short_tmp
    instance = "0" * 32
    env = {**os.environ, "XDG_RUNTIME_DIR": str(tmp_path)}
    host = subprocess.Popen(
        [sys.executable, "scripts/chrome_tabs.py", "host"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        env=env,
    )
    assert host.stdin is not None and host.stdout is not None
    try:
        write_frame(host.stdin, {"type": "hello", "instance": instance})
        socket_path = tmp_path / "chrome-tabs" / f"{instance}.sock"
        deadline = time.monotonic() + 10
        while not socket_path.exists() and time.monotonic() < deadline:
            time.sleep(0.02)

        # The host answers only after the fake extension does, so ask from a thread.
        answers: list[dict[str, object]] = []
        asker = threading.Thread(
            target=lambda: answers.append(ask_instance(socket_path, {"op": "list"}))
        )
        asker.start()
        request = read_frame(host.stdout)
        assert request is not None and request["op"] == "list"
        write_frame(
            host.stdin, {"id": request["id"], "ok": True, "result": {"tabs": []}}
        )
        asker.join(timeout=10)
        assert answers == [{"tabs": []}]
    finally:
        host.stdin.close()
        host.wait(timeout=10)
        host.stdout.close()
    assert not socket_path.exists(), (
        "the host removes its socket when Chrome closes the pipe"
    )


def test_host_refuses_an_instance_id_that_could_escape_the_directory(
    tmp_path: Path,
) -> None:
    frame = json.dumps({"type": "hello", "instance": "../../evil"}).encode()
    result = subprocess.run(
        [sys.executable, "scripts/chrome_tabs.py", "host"],
        input=struct.pack("=I", len(frame)) + frame,
        env={**os.environ, "XDG_RUNTIME_DIR": str(tmp_path)},
        capture_output=True,
        timeout=10,
        check=False,
    )
    assert result.returncode == 1
    assert list(tmp_path.rglob("*.sock")) == []


def read_crx_files() -> dict[str, bytes]:
    data = (EXTENSION / "chrome-tabs.crx").read_bytes()
    assert data[:4] == b"Cr24", "not a CRX file"
    (header_size,) = struct.unpack("<I", data[8:12])
    with zipfile.ZipFile(io.BytesIO(data[12 + header_size :])) as archive:
        return {name: archive.read(name) for name in archive.namelist()}


def test_packed_extension_matches_its_source_and_id() -> None:
    packed = read_crx_files()
    assert packed.keys() == {"manifest.json", "background.js"}, (
        "unexpected files in the CRX"
    )
    for name, content in packed.items():
        assert content == (EXTENSION / "src" / name).read_bytes(), (
            f"run pack.sh: {name} is stale"
        )
    match = re.fullmatch(r'"([a-p]{32})"\n', (EXTENSION / "_id.nix").read_text())
    assert match is not None


def test_policy_exempts_meet_and_teams() -> None:
    policy = Path("features/applications/chrome-policies.nix").read_text()
    for host in ("meet.google.com", "teams.microsoft.com", "teams.cloud.microsoft"):
        assert f'"{host}"' in policy
