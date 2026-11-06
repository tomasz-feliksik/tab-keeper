#!/usr/bin/env python3
"""List and unload Chrome tabs across every running profile.

Two roles in one file. `chrome-tabs` is the CLI. `chrome-tabs host` is the native
messaging host Chrome starts for each profile's bridge extension: it owns a Unix
socket under $XDG_RUNTIME_DIR/chrome-tabs/ and relays requests to the extension.
"""

from __future__ import annotations

import argparse
import itertools
import json
import os
import re
import socket
import struct
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import IO, cast
from urllib.parse import urlsplit

type Json = dict[str, object]

POLICY_PATH = Path("/etc/opt/chrome/policies/managed/nixos.json")
CHROME_DIR = Path.home() / ".config/google-chrome"
REQUEST_TIMEOUT_SECONDS = 10.0
DURATION_UNITS_MS = {"s": 1_000, "m": 60_000, "h": 3_600_000, "d": 86_400_000}
INSTANCE_PATTERN = re.compile(r"[0-9a-f]{32}")


class ChromeTabsError(Exception):
    """A failure to report to the user as one line, without a traceback."""


@dataclass(frozen=True)
class Tab:
    id: int
    title: str
    url: str
    active: bool
    pinned: bool
    audible: bool
    discarded: bool
    last_accessed_ms: float | None

    @classmethod
    def from_json(cls, raw: Json) -> Tab:
        last_accessed = raw.get("lastAccessed")
        return cls(
            id=cast(int, raw["id"]),
            title=cast(str, raw["title"]),
            url=cast(str, raw["url"]),
            active=cast(bool, raw["active"]),
            pinned=cast(bool, raw["pinned"]),
            audible=cast(bool, raw["audible"]),
            discarded=cast(bool, raw["discarded"]),
            last_accessed_ms=float(cast(float, last_accessed))
            if last_accessed is not None
            else None,
        )


@dataclass(frozen=True)
class Profile:
    instance: str
    name: str
    tabs: tuple[Tab, ...]


def parse_duration_ms(text: str) -> int:
    match = re.fullmatch(r"(\d+)([smhd])", text)
    if match is None:
        raise ChromeTabsError(
            f"bad duration {text!r}: use a number and s, m, h or d, like 2h"
        )
    return int(match.group(1)) * DURATION_UNITS_MS[match.group(2)]


def host_is_excepted(url: str, patterns: list[str]) -> bool:
    """Whether a URL's host equals, or is a subdomain of, an exception host."""
    host = urlsplit(url).hostname or ""
    return any(host == pattern or host.endswith(f".{pattern}") for pattern in patterns)


def discard_candidates(
    tabs: tuple[Tab, ...], *, idle_ms: int, now_ms: float, exceptions: list[str]
) -> list[Tab]:
    """Tabs safe to unload. A tab with no recorded access time is left alone."""
    return [
        tab
        for tab in tabs
        if not (tab.active or tab.pinned or tab.audible or tab.discarded)
        and tab.last_accessed_ms is not None
        and now_ms - tab.last_accessed_ms >= idle_ms
        and not host_is_excepted(tab.url, exceptions)
    ]


def read_discard_exceptions(policy_path: Path = POLICY_PATH) -> list[str]:
    """The hosts Memory Saver must not unload, shared with the CLI.

    A missing or unreadable policy stops the discard: running without the list
    could drop a call.
    """
    try:
        policy = cast(Json, json.loads(policy_path.read_text()))
        return cast(list[str], policy["TabDiscardingExceptions"])
    except (OSError, ValueError, KeyError) as error:
        raise ChromeTabsError(
            f"cannot read TabDiscardingExceptions from {policy_path}: {error}"
        ) from error


def profile_name_for_instance(
    instance: str, extension_id: str, chrome_dir: Path = CHROME_DIR
) -> str | None:
    """Find the profile directory whose extension storage holds this instance id.

    The extension cannot see its profile's name, and an account email does not
    identify one: profiles can share an account or have none. Its random instance
    id is unique, and chrome.storage.local writes it under that profile's directory.
    """
    needle = instance.encode()
    for path in chrome_dir.glob(f"*/Local Extension Settings/{extension_id}/*"):
        try:
            if needle in path.read_bytes():
                return path.relative_to(chrome_dir).parts[0]
        except OSError:
            continue  # LevelDB rotates its files; a vanished one holds nothing we need.
    return None


def write_frame(stream: IO[bytes], message: Json) -> None:
    """Chrome's native messaging framing: native-endian u32 length, then JSON."""
    body = json.dumps(message).encode()
    stream.write(struct.pack("=I", len(body)) + body)
    stream.flush()


def read_frame(stream: IO[bytes]) -> Json | None:
    """Read one frame, or None at end of stream."""
    header = stream.read(4)
    if len(header) < 4:
        return None
    (length,) = struct.unpack("=I", header)
    return cast(Json, json.loads(stream.read(length)))


def runtime_dir() -> Path:
    base = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return Path(base) / "chrome-tabs"


def run_host(stdin: IO[bytes], stdout: IO[bytes]) -> int:
    """Relay socket requests to the extension until Chrome closes the pipe."""
    hello = read_frame(stdin)
    if hello is None:
        return 1
    instance = str(hello.get("instance", ""))
    if INSTANCE_PATTERN.fullmatch(instance) is None:
        print(f"bad instance id from extension: {instance!r}", file=sys.stderr)
        return 1

    directory = runtime_dir()
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    socket_path = directory / f"{instance}.sock"
    socket_path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    listener.listen()

    pending: dict[int, socket.socket] = {}
    lock = threading.Lock()
    ids = itertools.count(1)

    def accept_requests() -> None:
        while True:
            connection, _ = listener.accept()
            with connection.makefile("rb") as reader:
                line = reader.readline()
            try:
                request = cast(Json, json.loads(line))
            except ValueError:
                connection.close()
                continue
            with lock:
                request_id = next(ids)
                pending[request_id] = connection
                write_frame(stdout, {**request, "id": request_id})

    threading.Thread(target=accept_requests, daemon=True).start()

    try:
        while (response := read_frame(stdin)) is not None:
            with lock:
                connection = pending.pop(cast(int, response["id"]), None)
            if connection is None:
                continue
            try:
                connection.sendall(json.dumps(response).encode())
            except OSError:
                pass  # The CLI timed out and went away; nobody is left to tell.
            connection.close()
    finally:
        socket_path.unlink(missing_ok=True)
    return 0


def ask_instance(socket_path: Path, request: Json) -> Json:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(REQUEST_TIMEOUT_SECONDS)
        client.connect(str(socket_path))
        client.sendall(json.dumps(request).encode() + b"\n")
        chunks: list[bytes] = []
        while chunk := client.recv(65536):
            chunks.append(chunk)
    response = cast(Json, json.loads(b"".join(chunks)))
    if not response.get("ok"):
        raise ChromeTabsError(f"extension error: {response.get('error')}")
    return cast(Json, response["result"])


def connected_profiles() -> list[Profile]:
    profiles: list[Profile] = []
    directory = runtime_dir()
    sockets = sorted(directory.glob("*.sock")) if directory.exists() else []
    for socket_path in sockets:
        try:
            result = ask_instance(socket_path, {"op": "list"})
        except ConnectionRefusedError:
            socket_path.unlink(missing_ok=True)  # Left by a host that died.
            continue
        except (OSError, ValueError) as error:
            print(f"warning: skipping {socket_path.name}: {error}", file=sys.stderr)
            continue
        extension_id = cast(str, result["extensionId"])
        tabs = tuple(Tab.from_json(raw) for raw in cast(list[Json], result["tabs"]))
        name = (
            profile_name_for_instance(socket_path.stem, extension_id)
            or socket_path.stem[:8]
        )
        profiles.append(Profile(socket_path.stem, name, tabs))
    if not profiles:
        raise ChromeTabsError(
            "no profile connected: is Chrome running with the chrome-tabs bridge extension?"
        )
    return profiles


def select_profiles(profiles: list[Profile], wanted: str | None) -> list[Profile]:
    if wanted is None:
        return profiles
    selected = [profile for profile in profiles if profile.name == wanted]
    if not selected:
        known = ", ".join(sorted(profile.name for profile in profiles))
        raise ChromeTabsError(
            f"no connected profile named {wanted!r}; connected: {known}"
        )
    return selected


def format_idle(tab: Tab, now_ms: float) -> str:
    if tab.last_accessed_ms is None:
        return "?"
    minutes = int((now_ms - tab.last_accessed_ms) / 60_000)
    if minutes < 60:
        return f"{minutes}m"
    if minutes < 1440:
        return f"{minutes // 60}h"
    return f"{minutes // 1440}d"


def format_flags(tab: Tab) -> str:
    states = {"A": tab.active, "P": tab.pinned, "S": tab.audible, "D": tab.discarded}
    return "".join(flag if state else "-" for flag, state in states.items())


def command_list(args: argparse.Namespace) -> int:
    profiles = select_profiles(connected_profiles(), cast("str | None", args.profile))
    if cast(bool, args.json):
        print(
            json.dumps(
                [
                    {"profile": p.name, "tabs": [t.__dict__ for t in p.tabs]}
                    for p in profiles
                ]
            )
        )
        return 0
    now_ms = time.time() * 1000
    for profile in profiles:
        unloaded = sum(tab.discarded for tab in profile.tabs)
        print(f"{profile.name}: {len(profile.tabs)} tabs, {unloaded} unloaded")
        for tab in profile.tabs:
            print(
                f"  {format_flags(tab)} {format_idle(tab, now_ms):>4}  {tab.title[:70]}"
            )
    return 0


def command_discard(args: argparse.Namespace) -> int:
    idle_ms = parse_duration_ms(cast(str, args.idle))
    exceptions = read_discard_exceptions()
    dry_run = cast(bool, args.dry_run)
    profiles = select_profiles(connected_profiles(), cast("str | None", args.profile))
    now_ms = time.time() * 1000
    failures = 0
    for profile in profiles:
        candidates = discard_candidates(
            profile.tabs, idle_ms=idle_ms, now_ms=now_ms, exceptions=exceptions
        )
        if dry_run:
            print(f"{profile.name}: would unload {len(candidates)} tabs")
            for tab in candidates:
                print(f"  {format_idle(tab, now_ms):>4}  {tab.title[:70]}")
            continue
        if not candidates:
            print(f"{profile.name}: nothing to unload")
            continue
        socket_path = runtime_dir() / f"{profile.instance}.sock"
        result = ask_instance(
            socket_path, {"op": "discard", "tabIds": [tab.id for tab in candidates]}
        )
        discarded = cast(list[int], result["discarded"])
        failed = cast(list[Json], result["failed"])
        failures += len(failed)
        print(f"{profile.name}: unloaded {len(discarded)} tabs, {len(failed)} failed")
        for failure in failed:
            print(f"  tab {failure['id']}: {failure['error']}", file=sys.stderr)
    return 1 if failures else 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="chrome-tabs", description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    list_parser = commands.add_parser(
        "list", help="show tabs; flags are Active, Pinned, Sound, Discarded"
    )
    list_parser.add_argument(
        "--profile", help="only this profile directory, like Default"
    )
    list_parser.add_argument("--json", action="store_true")
    list_parser.set_defaults(handler=command_list)

    discard_parser = commands.add_parser(
        "discard",
        help="unload idle tabs, skipping active, pinned, audible and policy-exempt ones",
    )
    discard_parser.add_argument(
        "--idle", required=True, help="minimum idle time, like 30m or 2h"
    )
    discard_parser.add_argument("--profile", help="only this profile directory")
    discard_parser.add_argument("--dry-run", action="store_true")
    discard_parser.set_defaults(handler=command_discard)
    return parser


def main(argv: list[str]) -> int:
    # Chrome starts a native host with its extension origin as the only argument.
    if argv[:1] == ["host"]:
        return run_host(sys.stdin.buffer, sys.stdout.buffer)
    args = build_parser().parse_args(argv)
    try:
        return cast(int, args.handler(args))
    except ChromeTabsError as error:
        print(f"chrome-tabs: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
