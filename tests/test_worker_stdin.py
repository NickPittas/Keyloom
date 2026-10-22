"""PolledStdin (Windows stdin deadlock fix) with a mocked pipe; the real Windows run is separate."""
from __future__ import annotations

import ctypes
import sys
import types

import pytest

import backend.worker as w
from backend.worker import MAX_LINE, PolledStdin


class FakePipe:
    """peek()/read() over scripted chunks; ``end`` selects how the stream finishes."""

    def __init__(self, chunks, end="broken"):
        self.chunks, self.end, self.sleeps, self.reads, self.peeks = list(chunks), end, 0, [], 0

    def peek(self):
        self.peeks += 1
        if self.chunks and self.chunks[0] is None:  # writer is idle for a moment
            self.chunks.pop(0)
            return 0
        if not self.chunks:
            if self.end == "broken":
                raise EOFError(109)
            if self.end == "error":
                raise OSError(None, "boom", None, 6)
            return 0  # idle forever (would poll forever)
        return len(self.chunks[0])

    def read(self, fd, n):
        self.reads.append(n)
        c = self.chunks[0]
        out, self.chunks[0] = c[:n], c[n:]
        if not self.chunks[0]:
            self.chunks.pop(0)
        return out

    def sleep(self, s):
        self.sleeps += 1

    def stdin(self, **kw):
        return PolledStdin(0, peek=self.peek, read=self.read, sleep=self.sleep, **kw)


def test_lines_split_and_idle_polls_without_blocking_read():
    p = FakePipe([b'{"a":1}\n{"b"', None, None, b':2}\n'])
    s = p.stdin()
    assert s.readline() == b'{"a":1}\n'
    assert s.readline() == b'{"b":2}\n'
    assert p.sleeps == 2 and all(n > 0 for n in p.reads)  # never a read when no data is waiting
    assert s.readline() == b""  # broken pipe == EOF


def test_eof_returns_partial_line_then_empty():
    s = FakePipe([b"tail-without-newline"]).stdin()
    assert s.readline() == b"tail-without-newline"
    assert s.readline() == b""


def test_read_never_asks_for_more_than_available_or_64k():
    p = FakePipe([b"x" * 10, b"y" * 70000 + b"\n"])
    s = p.stdin()
    assert s.readline() == b"x" * 10 + b"y" * 70000 + b"\n"
    assert p.reads[0] == 10 and max(p.reads) == 65536


@pytest.mark.parametrize("data,limit,expect", [
    (b"abc\ndef\n", 3, b"abc"),        # newline sits at index 3: outside the limit
    (b"abc\ndef\n", 4, b"abc\n"),      # newline is the last allowed byte
    (b"abc\ndef\n", 100, b"abc\n"),
    (b"abcdef", 4, b"abcd"),
    (b"\n", 1, b"\n"),
])
def test_limit_caps_the_newline_position(data, limit, expect):
    s = FakePipe([data]).stdin()
    assert s.readline(limit) == expect
    assert s.readline(100) in (data[len(expect):].split(b"\n")[0] + (b"\n" if b"\n" in data[len(expect):] else b""),)


def test_limit_zero_returns_empty_without_io():
    p = FakePipe([b"abc\n"])
    assert p.stdin().readline(0) == b"" and p.reads == [] and p.peeks == 0


def test_overlong_line_is_bounded_by_max_line_and_buffer_stays_small():
    big = b"z" * (MAX_LINE + 1000)  # no newline at all
    p = FakePipe([big])
    s = p.stdin()
    first = s.readline(MAX_LINE + 1)
    assert len(first) == MAX_LINE + 1 and b"\n" not in first
    assert len(s._buf) <= 65536  # leftover never exceeds one read chunk


def test_default_limit_is_max_line_plus_one():
    s = FakePipe([b"q" * (MAX_LINE + 5)]).stdin()
    assert len(s.readline()) == MAX_LINE + 1


def test_unexpected_peek_error_raises_and_never_falls_back_to_blocking_read():
    p = FakePipe([b"partial"], end="error")
    s = p.stdin()
    assert s.readline(3) == b"par"  # served from what was already read
    p.chunks.clear()
    with pytest.raises(OSError):
        s.readline(100)
    assert p.reads == [7]  # exactly the one read that had data


def test_non_pipe_stdin_uses_plain_blocking_reads():
    reads = []
    s = PolledStdin(0, peek=None, read=lambda fd, n: (reads.append(n), b"line\nrest")[1] if not reads else b"", sleep=None)
    s._peek = None
    assert s.readline() == b"line\n" and s.readline() == b"rest" and s.readline() == b""


# -------- the ctypes layer, against a fake kernel32 (no Windows needed)

class FakeK32:
    def __init__(self, ok, avail=0, err=0):
        self.ok, self.avail, self.err, self.calls = ok, avail, err, []

        def peek(handle, buf, n, got, avail_ref, left):
            self.calls.append((handle, buf, n, got, left))
            avail_ref._obj.value = self.avail
            return self.ok
        self.PeekNamedPipe = peek


def make_peek(k32, err=0, handle=0x1_0000_0001):
    from ctypes import wintypes
    return w._win_pipe_peek(k32, lambda: err, wintypes.HANDLE(handle))


def test_peek_declares_argtypes_and_restype_for_64_bit_handles():
    from ctypes import wintypes
    k = FakeK32(True, 5)
    make_peek(k)
    f = k.PeekNamedPipe
    assert f.argtypes[0] is wintypes.HANDLE and len(f.argtypes) == 6
    assert f.restype is wintypes.BOOL


def test_peek_reports_available_bytes_and_passes_the_full_handle():
    k = FakeK32(True, 42)
    assert make_peek(k)() == 42
    assert k.calls[0][0].value == 0x1_0000_0001  # not truncated to 32 bits


@pytest.mark.parametrize("err", [109, 232])
def test_peek_broken_or_closing_pipe_is_eof(err):
    with pytest.raises(EOFError):
        make_peek(FakeK32(False, err=err), err=err)()


@pytest.mark.parametrize("err", [6, 87, 5, 0])
def test_peek_other_errors_are_oserror_with_winerror(err):
    with pytest.raises(OSError) as exc:
        make_peek(FakeK32(False), err=err)()
    assert exc.value.winerror == err if hasattr(exc.value, "winerror") and sys.platform == "win32" else True
    assert str(err) in str(exc.value)


# -------- serve() integration

def test_serve_treats_a_stdin_oserror_as_eof_and_cancels(tmp_path):
    from tests.test_worker_protocol import HELLO, Sink
    out = Sink()
    worker = w.Worker(object(), out, HELLO)

    class Broken:
        def readline(self, n):
            raise OSError(None, "handle invalid", None, 6)
    assert worker.serve(Broken()) == 0
    assert [e["event"] for e in out.events()] == ["hello"]


def test_main_uses_polled_stdin_only_on_windows(monkeypatch):
    seen = {}
    monkeypatch.setattr(w.Worker, "serve", lambda self, stdin: seen.setdefault("stdin", stdin) and 0)
    monkeypatch.setattr(w, "PolledStdin", lambda fd: ("polled", fd))
    from tests.test_worker_protocol import HELLO, Sink
    monkeypatch.setattr(sys, "platform", "win32")
    w.main([], out=Sink(), service=object(), hello=HELLO)
    assert seen["stdin"] == ("polled", 0)
    seen.clear()
    monkeypatch.setattr(sys, "platform", "linux")
    w.main([], out=Sink(), service=object(), hello=HELLO)
    assert seen["stdin"] is sys.stdin.buffer
