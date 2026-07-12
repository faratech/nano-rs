#!/usr/bin/env python3
"""Exercise nano's byte-integrity and terminal-EOF behavior through a real PTY.

The fast profile is intended for pull requests.  The broad profile adds large
and seeded payloads for scheduled/manual runs.  The harness has no third-party
dependencies and deliberately talks to nano as a terminal would: bracketed
paste is not sent until nano enables it, and every os.write() is retried until
the requested bytes have actually reached the PTY.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import random
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from dataclasses import dataclass, field
from typing import Sequence


PASTE_BEGIN = b"\x1b[200~"
PASTE_END = b"\x1b[201~"
PASTE_ENABLED = b"\x1b[?2004h"
CTRL_X = b"\x18"
DEFAULT_SEED = 0x4E414E4F  # ASCII "NANO"
ROWS = 30
COLS = 100


def parse_seed(value: str) -> int:
    try:
        return int(value, 0)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            "seed must be a decimal integer or use a prefix such as 0x"
        ) from exc


def resolve_executable(value: str) -> str:
    if os.sep in value:
        path = Path(value).expanduser().resolve()
        if not path.is_file():
            raise ValueError(f"executable does not exist: {path}")
        if not os.access(path, os.X_OK):
            raise ValueError(f"file is not executable: {path}")
        return str(path)
    found = shutil.which(value)
    if not found:
        raise ValueError(f"executable is not on PATH: {value}")
    return str(Path(found).resolve())


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def executable_identity(command: Sequence[str]) -> dict[str, object]:
    if not command:
        return {}
    path = Path(command[0])
    version = ""
    try:
        completed = subprocess.run(
            [str(path), "--version"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=2.0,
            check=False,
        )
        version = completed.stdout.decode("utf-8", "replace")
    except (OSError, subprocess.SubprocessError):
        pass
    return {
        "path": str(path),
        "sha256": file_sha256(path) if path.is_file() else None,
        "version": version,
    }


def normalized_paste(payload: bytes) -> bytes:
    """GNU nano treats every CR and every LF in a paste as a line break."""

    return payload.replace(b"\r", b"\n")


def valid_html_payload(size: int) -> bytes:
    """Return exactly *size* bytes of deterministic, valid UTF-8 HTML-like data."""

    if size < 0:
        raise ValueError("payload size cannot be negative")
    header = b"<!doctype html>\n<meta charset=\"utf-8\">\n<script>\n"
    row = (
        "const cat = { name: 'Mochi 猫', mood: '🐾', "
        "note: 'combining e\u0301' }; // <>& tabs\tstay\n"
    ).encode("utf-8")
    footer = b"</script>\n"
    data = bytearray()
    data.extend(header[:size])
    while len(data) + len(row) + len(footer) <= size:
        data.extend(row)
    if len(data) < size and size - len(data) >= len(footer):
        room = size - len(data) - len(footer)
        # Use only complete UTF-8 sequences from the next row, then fill the
        # exact requested boundary with ASCII so every case stays valid UTF-8.
        prefix = row[:room]
        while prefix:
            try:
                prefix.decode("utf-8")
                break
            except UnicodeDecodeError:
                prefix = prefix[:-1]
        data.extend(prefix)
        data.extend(b"x" * (room - len(prefix)))
        data.extend(footer)
    else:
        data.extend(b"x" * (size - len(data)))
    result = bytes(data)
    assert len(result) == size
    result.decode("utf-8")
    return result


def seeded_safe_payload(rng: random.Random, size: int) -> bytes:
    """Generate paste-safe bytes, including malformed UTF-8 and line breaks."""

    safe = [9, 10, 13, *range(32, 127), *range(128, 256)]
    return bytes(rng.choice(safe) for _ in range(size))


@dataclass(frozen=True)
class PasteCase:
    name: str
    payload: bytes
    chunks: tuple[int, ...]
    marker_chunks: tuple[int, ...] = ()
    marker_delay: float = 0.0

    @property
    def expected(self) -> bytes:
        return normalized_paste(self.payload)


@dataclass
class RunResult:
    ok: bool
    returncode: int | None
    actual: bytes | None
    transcript: bytes
    elapsed: float
    error: str = ""
    command: list[str] = field(default_factory=list)


class PtySession:
    def __init__(
        self,
        executable: str,
        args: Sequence[str],
        cwd: Path,
        env: dict[str, str],
    ) -> None:
        master, slave = os.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", ROWS, COLS, 0, 0),
        )
        os.set_blocking(master, False)
        self.master = master
        self.transcript = bytearray()
        self.command = [executable, *args]
        try:
            self.process = subprocess.Popen(
                self.command,
                stdin=slave,
                stdout=slave,
                stderr=slave,
                cwd=cwd,
                env=env,
                close_fds=True,
                start_new_session=True,
            )
        finally:
            os.close(slave)

    def _read_available(self) -> bytes:
        if self.master < 0:
            return b""
        pieces: list[bytes] = []
        while True:
            try:
                chunk = os.read(self.master, 65536)
            except BlockingIOError:
                break
            except OSError as exc:
                if exc.errno in (errno.EIO, errno.EBADF):
                    break
                raise
            if not chunk:
                break
            pieces.append(chunk)
            self.transcript.extend(chunk)
        return b"".join(pieces)

    def wait_for(self, needle: bytes, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._read_available()
            if needle in self.transcript:
                return True
            if self.process.poll() is not None:
                self._read_available()
                return needle in self.transcript
            wait = min(0.05, max(0.0, deadline - time.monotonic()))
            if self.master >= 0:
                select.select([self.master], [], [], wait)
        self._read_available()
        return needle in self.transcript

    def drain_for(self, duration: float) -> None:
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            self._read_available()
            if self.process.poll() is not None:
                break
            wait = min(0.03, max(0.0, deadline - time.monotonic()))
            if self.master >= 0:
                select.select([self.master], [], [], wait)
        self._read_available()

    def write_all(self, data: bytes, timeout: float = 10.0) -> None:
        """Write all bytes, handling short/EAGAIN writes and draining output."""

        if self.master < 0:
            raise RuntimeError("PTY master is closed")
        view = memoryview(data)
        deadline = time.monotonic() + timeout
        while view:
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"editor exited with {self.process.returncode} during input"
                )
            if time.monotonic() >= deadline:
                raise TimeoutError(f"timed out with {len(view)} input bytes unwritten")
            readable, writable, _ = select.select(
                [self.master], [self.master], [], min(0.05, deadline - time.monotonic())
            )
            if readable:
                self._read_available()
            if not writable:
                continue
            try:
                count = os.write(self.master, view)
            except BlockingIOError:
                continue
            except InterruptedError:
                continue
            if count <= 0:
                raise RuntimeError(f"PTY write made no progress: {count}")
            view = view[count:]

    def write_chunked(self, data: bytes, chunks: Sequence[int]) -> None:
        if not chunks:
            self.write_all(data)
            return
        offset = 0
        index = 0
        while offset < len(data):
            width = max(1, chunks[index % len(chunks)])
            piece = data[offset : offset + width]
            self.write_all(piece)
            offset += len(piece)
            index += 1

    def write_fragmented(
        self, data: bytes, chunks: Sequence[int], delay: float = 0.0
    ) -> None:
        """Write according to a transport-fragment schedule."""

        if not chunks:
            self.write_all(data)
            return
        offset = 0
        index = 0
        while offset < len(data):
            width = max(1, chunks[index % len(chunks)])
            piece = data[offset : offset + width]
            self.write_all(piece)
            offset += len(piece)
            index += 1
            if delay and offset < len(data):
                time.sleep(delay)

    def close_master(self) -> None:
        if self.master >= 0:
            os.close(self.master)
            self.master = -1

    def wait(self, timeout: float) -> int | None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self._read_available()
            rc = self.process.poll()
            if rc is not None:
                self._read_available()
                return rc
            wait = min(0.03, max(0.0, deadline - time.monotonic()))
            if self.master >= 0:
                select.select([self.master], [], [], wait)
            else:
                time.sleep(wait)
        return self.process.poll()

    def terminate(self) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=0.5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    self.process.wait(timeout=1.0)
                except subprocess.TimeoutExpired:
                    pass
        self.close_master()


def hermetic_env(home: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.update(
        {
            "TERM": "xterm-256color",
            "LC_ALL": "C.UTF-8",
            "LANG": "C.UTF-8",
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / "xdg-config"),
            "XDG_CACHE_HOME": str(home / "xdg-cache"),
            "XDG_DATA_HOME": str(home / "xdg-data"),
        }
    )
    for key in ("NANORC", "NO_COLOR", "COLORTERM"):
        env.pop(key, None)
    return env


def run_paste_case(executable: str, case: PasteCase) -> RunResult:
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="nano-pty-paste-") as temp:
        root = Path(temp)
        home = root / "home"
        home.mkdir()
        target = root / "payload.bin"
        target.write_bytes(b"")
        session = PtySession(
            executable,
            ["-I", "-L", "-t", str(target)],
            root,
            hermetic_env(home),
        )
        error = ""
        try:
            if not session.wait_for(PASTE_ENABLED, 5.0):
                error = "editor did not enable bracketed paste"
            else:
                before_paste_output = len(session.transcript)
                session.write_fragmented(
                    PASTE_BEGIN, case.marker_chunks, case.marker_delay
                )
                session.write_chunked(case.payload, case.chunks)
                session.write_fragmented(PASTE_END, case.marker_chunks, case.marker_delay)
                # A redraw proves the editor consumed the complete paste event.
                # Without this acknowledgement Ctrl-X could race an incomplete
                # delimiter through the PTY and turn a parser defect into a
                # nondeterministic timeout.
                if case.payload:
                    deadline = time.monotonic() + 1.0
                    while (
                        len(session.transcript) == before_paste_output
                        and time.monotonic() < deadline
                        and session.process.poll() is None
                    ):
                        session.drain_for(0.03)
                else:
                    session.drain_for(0.08)
                session.write_all(CTRL_X)
                rc = session.wait(5.0)
                if rc is None:
                    error = "editor did not exit after Ctrl-X"
                elif rc != 0:
                    error = f"editor exited with status {rc}"
                actual = target.read_bytes() if target.exists() else None
                if not error and actual != case.expected:
                    error = (
                        "saved bytes differ: "
                        f"expected {len(case.expected)} bytes/{sha256(case.expected)}, "
                        f"got {len(actual or b'')} bytes/{sha256(actual or b'')}"
                    )
                return RunResult(
                    ok=not error,
                    returncode=rc,
                    actual=actual,
                    transcript=bytes(session.transcript),
                    elapsed=time.monotonic() - started,
                    error=error,
                    command=session.command,
                )
        except Exception as exc:  # preserve diagnostics from a hostile PTY run
            error = f"{type(exc).__name__}: {exc}"
        finally:
            session.terminate()
        actual = target.read_bytes() if target.exists() else None
        return RunResult(
            ok=False,
            returncode=session.process.poll(),
            actual=actual,
            transcript=bytes(session.transcript),
            elapsed=time.monotonic() - started,
            error=error,
            command=session.command,
        )


def process_ticks(pid: int) -> int | None:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text(encoding="ascii").split()
        return int(fields[13]) + int(fields[14])
    except (FileNotFoundError, IndexError, ValueError, OSError):
        return None


def wait_after_master_close(session: PtySession, timeout: float) -> tuple[int | None, int | None]:
    before = process_ticks(session.process.pid)
    deadline = time.monotonic() + timeout
    while session.process.poll() is None and time.monotonic() < deadline:
        time.sleep(0.025)
    after = process_ticks(session.process.pid)
    delta = None if before is None or after is None else after - before
    return session.process.poll(), delta


def run_eof_case(
    executable: str, modified: bool, non_utf8_name: bool = False
) -> tuple[RunResult, bytes | None]:
    started = time.monotonic()
    payload = b"emergency-save\nsentinel" if modified else b""
    with tempfile.TemporaryDirectory(prefix="nano-pty-eof-") as temp:
        root = Path(temp)
        home = root / "home"
        home.mkdir()
        target = root / (os.fsdecode(b"eof-\xff.txt") if non_utf8_name else "eof.txt")
        target.write_bytes(b"" if modified else b"unchanged\n")
        session = PtySession(
            executable,
            ["-I", "-L", str(target)],
            root,
            hermetic_env(home),
        )
        error = ""
        saved: bytes | None = None
        tick_delta: int | None = None
        try:
            if not session.wait_for(PASTE_ENABLED, 5.0):
                error = "editor did not enable bracketed paste"
            elif modified:
                session.write_chunked(PASTE_BEGIN + payload + PASTE_END, (1, 2, 7, 31))
                session.drain_for(0.15)
            session.close_master()
            rc, tick_delta = wait_after_master_close(session, 1.5)
            if rc is None:
                detail = "unknown"
                if tick_delta is not None:
                    detail = str(tick_delta)
                error = f"editor did not exit after PTY EOF (CPU tick delta: {detail})"
            elif rc == 0:
                error = "editor reported success after fatal PTY EOF"

            save_files = sorted(root.glob(target.name + ".save*"))
            if modified:
                if len(save_files) != 1:
                    error = error or f"expected one emergency save, found {len(save_files)}"
                else:
                    saved = save_files[0].read_bytes()
                    if saved != payload:
                        error = error or (
                            "emergency save differs: "
                            f"expected {len(payload)} bytes/{sha256(payload)}, "
                            f"got {len(saved)} bytes/{sha256(saved)}"
                        )
            elif save_files:
                error = error or "unmodified buffer created an emergency save"

            if tick_delta is not None and tick_delta > 10:
                # This is mainly diagnostic when the process did exit.  A
                # process still alive is already a hard failure above.
                error = error or f"PTY EOF consumed {tick_delta} CPU ticks before exit"

            return (
                RunResult(
                    ok=not error,
                    returncode=rc,
                    actual=saved,
                    transcript=bytes(session.transcript),
                    elapsed=time.monotonic() - started,
                    error=error,
                    command=session.command,
                ),
                payload if modified else None,
            )
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"
            return (
                RunResult(
                    ok=False,
                    returncode=session.process.poll(),
                    actual=saved,
                    transcript=bytes(session.transcript),
                    elapsed=time.monotonic() - started,
                    error=error,
                    command=session.command,
                ),
                payload if modified else None,
            )
        finally:
            session.terminate()


class ArtifactWriter:
    def __init__(self, root: Path, seed: int, profile: str) -> None:
        self.root = root
        self.seed = seed
        self.profile = profile

    def write(
        self,
        role: str,
        name: str,
        result: RunResult,
        payload: bytes | None,
        expected: bytes | None,
        details: dict[str, object] | None = None,
    ) -> Path:
        slug = re.sub(r"[^A-Za-z0-9_.-]+", "-", f"{role}-{name}").strip("-")
        destination = self.root / slug
        suffix = 2
        while destination.exists():
            destination = self.root / f"{slug}-{suffix}"
            suffix += 1
        destination.mkdir(parents=True)
        if payload is not None:
            (destination / "input.bin").write_bytes(payload)
        if expected is not None:
            (destination / "expected.bin").write_bytes(expected)
        if result.actual is not None:
            (destination / "actual.bin").write_bytes(result.actual)
        (destination / "terminal-transcript.bin").write_bytes(result.transcript)
        metadata = {
            "role": role,
            "case": name,
            "profile": self.profile,
            "seed": self.seed,
            "seed_hex": hex(self.seed),
            "command": result.command,
            "returncode": result.returncode,
            "elapsed_seconds": result.elapsed,
            "error": result.error,
            "input_bytes": None if payload is None else len(payload),
            "input_sha256": None if payload is None else sha256(payload),
            "expected_bytes": None if expected is None else len(expected),
            "expected_sha256": None if expected is None else sha256(expected),
            "actual_bytes": None if result.actual is None else len(result.actual),
            "actual_sha256": None if result.actual is None else sha256(result.actual),
            "platform": sys.platform,
            "uname": list(os.uname()) if hasattr(os, "uname") else None,
            "python": sys.version,
            "terminal_rows": ROWS,
            "terminal_columns": COLS,
            "executable": executable_identity(result.command),
            "details": details or {},
        }
        (destination / "metadata.json").write_text(
            json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        return destination


def paste_cases(profile: str, seed: int) -> list[PasteCase]:
    common_chunks = (1, 2, 511, 512, 1023, 4095, 4096, 8191, 8192)
    sizes = [511, 512, 1023, 1024, 4095, 4096, 8191, 8192, 8193, 16384, 65536, 87464]
    cases = [
        PasteCase(f"html-{size}", valid_html_payload(size), common_chunks)
        for size in sizes
    ]
    cases.extend(
        [
            PasteCase("empty", b"", (1,)),
            PasteCase("line-endings-lf", b"alpha\nbeta\n", (1, 2)),
            PasteCase("line-endings-cr", b"alpha\rbeta\r", (2, 1)),
            PasteCase("line-endings-crlf", b"alpha\r\nbeta\r\n", (1, 3, 2)),
            PasteCase(
                "line-endings-mixed",
                b"one\rtwo\nthree\r\nfour",
                (5, 1, 7, 2),
            ),
            PasteCase(
                "invalid-high-bytes",
                b"prefix\t\x80\x81\xbf\xc0\xc1\xf5\xfe\xff\nsuffix",
                (1, 2, 3, 7),
            ),
            PasteCase("all-high-bytes", bytes(range(128, 256)), (1, 17, 31)),
            PasteCase(
                "utf8-byte-fragments",
                "split 🐾 猫 e\u0301 across every byte".encode("utf-8"),
                (1,),
            ),
            # Force separate short reads of both delimiters.  Crossterm used
            # to decide that a one-byte read containing ESC was a complete
            # Escape key, losing the bracketed-paste marker.  A small fixed
            # delay makes that race deterministic and keeps failure artifacts
            # reproducible.
            PasteCase(
                "fragmented-delimiters",
                b"marker-fragment-sentinel",
                (3, 1, 7),
                marker_chunks=(1,),
                marker_delay=0.01,
            ),
        ]
    )
    if profile == "broad":
        broad_sizes = [
            1,
            2,
            255,
            256,
            4097,
            16383,
            16385,
            32768,
            65535,
            65537,
            131072,
            262144,
            1048576,
        ]
        cases.extend(
            PasteCase(f"html-broad-{size}", valid_html_payload(size), common_chunks)
            for size in broad_sizes
        )
        rng = random.Random(seed)
        for index in range(8):
            size = rng.randint(1, 131072)
            chunks = tuple(rng.choice(common_chunks) for _ in range(7))
            cases.append(
                PasteCase(
                    f"seeded-{index:02d}-{size}",
                    seeded_safe_payload(rng, size),
                    chunks,
                )
            )
    return cases


def print_result(role: str, name: str, result: RunResult) -> None:
    status = "PASS" if result.ok else "FAIL"
    print(f"[{status}] {role:9s} {name:28s} {result.elapsed:6.2f}s", flush=True)
    if result.error:
        print(f"       {result.error}", flush=True)


def run_suite(args: argparse.Namespace) -> int:
    candidate = resolve_executable(args.candidate)
    reference = resolve_executable(args.reference) if args.reference else None
    if reference and os.path.samefile(candidate, reference):
        raise ValueError("candidate and reference resolve to the same executable")

    print(
        f"PTY integrity profile={args.profile} seed={hex(args.seed)} "
        f"candidate={candidate} reference={reference or 'none'}",
        flush=True,
    )
    artifacts = ArtifactWriter(args.artifact_dir, args.seed, args.profile)
    failures = 0

    for case in paste_cases(args.profile, args.seed):
        for role, executable in (("candidate", candidate), ("reference", reference)):
            if executable is None:
                continue
            result = run_paste_case(executable, case)
            print_result(role, case.name, result)
            if not result.ok:
                failures += 1
                where = artifacts.write(
                    role,
                    case.name,
                    result,
                    case.payload,
                    case.expected,
                    {
                        "payload_chunks": list(case.chunks),
                        "marker_chunks": list(case.marker_chunks),
                        "marker_delay_seconds": case.marker_delay,
                    },
                )
                print(f"       artifacts: {where}", flush=True)

    # The EOF regression is a candidate guard, not a platform-version parity
    # assertion.  Reference nano behavior can vary in status text while the
    # byte-oriented paste oracle above remains stable.
    eof_cases = (
        ("eof-unmodified", False, False),
        ("eof-modified", True, False),
        ("eof-modified-non-utf8-name", True, True),
    )
    for name, modified, non_utf8_name in eof_cases:
        result, expected = run_eof_case(candidate, modified, non_utf8_name)
        print_result("candidate", name, result)
        if not result.ok:
            failures += 1
            where = artifacts.write(
                "candidate",
                name,
                result,
                expected,
                expected,
                {"master_close": True, "non_utf8_filename": non_utf8_name},
            )
            print(f"       artifacts: {where}", flush=True)

    if failures:
        print(f"PTY integrity failed: {failures} case(s)", file=sys.stderr)
        return 1
    print("PTY integrity passed", flush=True)
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="verify nano paste bytes and PTY EOF handling"
    )
    result.add_argument(
        "--candidate",
        required=True,
        help="nano-rs executable to test",
    )
    result.add_argument(
        "--reference",
        help="optional GNU nano executable checked against the same paste oracle",
    )
    result.add_argument(
        "--profile",
        choices=("fast", "broad"),
        default="fast",
        help="fast PR suite or broader scheduled suite",
    )
    result.add_argument(
        "--seed",
        type=parse_seed,
        default=DEFAULT_SEED,
        help="seed for broad generated cases (default: 0x4E414E4F)",
    )
    result.add_argument(
        "--artifact-dir",
        type=Path,
        default=Path("artifacts/pty-integrity"),
        help="directory populated with diagnostics only for failed cases",
    )
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        return run_suite(args)
    except (OSError, ValueError) as exc:
        print(f"pty_integrity.py: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
