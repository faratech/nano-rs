# WSL PTY short writes can truncate large terminal pastes

This note records a data-loss incident observed while pasting a large HTML file
into nano from Windows Terminal through WSL.  The missing bytes were discarded
by the WSL input relay before they reached the editor.  The incident is not a
newline-conversion problem, and nano cannot reconstruct data that is absent
from its input stream.

## Forensic result

The observed and recovered files were:

| Artifact | Bytes | LF bytes | SHA-256 |
|---|---:|---:|---|
| Observed `index.html` | 65,448 | 1,064 | `39d2ec49020379677a795ff057b84330dd5fda2bb27413723524d03550d2dfc9` |
| Recovered reference | 87,464 | 1,389 | `266742a383c3958f6409ee1dd86597127431549dd671d9a87cd43e2b6ac930cd` |

Both files were valid UTF-8 and used LF line endings.  The observed file was an
exact subsequence of the reference: there were no substitutions or newline
rewrites, only these nine half-open byte ranges missing from the reference:

| Reference range `[start, end)` | Missing bytes | `end + 6` |
|---:|---:|---:|
| `[15866, 16378)` | 512 | 16,384 |
| `[24058, 24570)` | 512 | 24,576 |
| `[32250, 32762)` | 512 | 32,768 |
| `[37370, 40954)` | 3,584 | 40,960 |
| `[45562, 49146)` | 3,584 | 49,152 |
| `[54778, 57338)` | 2,560 | 57,344 |
| `[61946, 65530)` | 3,584 | 65,536 |
| `[70138, 73722)` | 3,584 | 73,728 |
| `[78330, 81914)` | 3,584 | 81,920 |

The ranges total 22,016 bytes.  Bracketed paste adds the six-byte `ESC [ 2 0 0 ~`
prefix before the document payload.  Adding those six bytes makes every missing
range end on an exact 8,192-byte input-stream boundary.  In the affected 8 KiB
blocks, the retained prefixes were respectively 7,680, 7,680, 7,680, 4,608,
4,608, 5,632, 4,608, 4,608, and 4,608 bytes.  This regularity is inconsistent
with random file corruption or newline handling and is characteristic of
discarded suffixes from short writes.

## Root cause in WSL 2.9.3

The affected session used WSL 2.9.3 and its `/init` relay.  In the tagged WSL
source, [`StdIn` is made nonblocking][wsl-relay] before the relay loop.  A write
to a nonblocking PTY can legally succeed while accepting fewer bytes than were
requested.

The delayed-input path at lines 1759-1772 correctly removes only the number of
bytes actually written from `PendingStdin`.  However, the immediate
socket-to-PTY path at lines 1823-1842 calls:

```cpp
BytesWritten = write(StdIn, Buffer.data(), BytesRead);
```

and reacts only when `BytesWritten < 0`.  It queues the whole buffer for
`EAGAIN`/`EWOULDBLOCK`, but has no branch for
`0 <= BytesWritten < BytesRead`.  A nonnegative short write is therefore treated
as complete and the unwritten suffix is dropped.  Each missing range above ends
where the relay finished one 8 KiB input block, matching that failure mode.

[wsl-relay]: https://github.com/microsoft/WSL/blob/2.9.3/src/linux/init/init.cpp#L1715-L1843

## Why nano cannot repair this loss

Nano sees only the bytes successfully written to its PTY.  The omitted suffixes
do not appear as an error, replacement character, or malformed bracketed-paste
sequence: later blocks and the paste terminator still arrive normally.  A text
editor cannot infer which arbitrary bytes belonged between two valid portions
of a stream.  Throttling nano's reader would not make this reliable and could
increase PTY backpressure.

Until the relay is fixed, bypass terminal input for large or irreplaceable
content:

- Transfer the content as a regular file with `git`, `scp`, `curl`, a mounted
  Windows file, or another file-copy mechanism, then open that file in nano.
- If a producer runs inside WSL, redirect it to a file and edit the result, for
  example `producer > page.html && nano page.html`.  This does not pass the
  payload through the Windows-terminal stdin relay.
- Compare a checksum or byte count after any cross-environment transfer.  Do
  not rely on splitting a terminal paste into smaller chunks as a data-integrity
  guarantee; the available PTY capacity depends on timing and buffer state.

Avoid clipboard-to-file tools that silently change encoding or line endings
when byte identity matters.

## Upstream fix

The immediate path should preserve the unwritten suffix exactly as the delayed
path does.  After a positive short write, it can assign
`Buffer[BytesWritten..BytesRead]` to `PendingStdin`; after `EAGAIN` it should
queue the full range.  Alternatively, it can advance through the buffer in a
loop, stopping only when all bytes are written or the nonblocking descriptor
would block, then queueing the remainder.  The implementation should also retry
`EINTR` without losing or duplicating bytes.

Tests for the relay should force partial writes, including a zero-byte short
write, across multiple 8 KiB reads and assert that the output stream is
byte-for-byte identical to the input.  The test must cover later successful
retries so it can detect lost, duplicated, or reordered suffixes.

## Separate nano-rs defects

This WSL defect is independent of nano-rs issues found during the same audit:

- nano-rs used a UTF-8 `String` for document content and could not fully edit
  and save arbitrary byte sequences;
- its terminal paste path converted input through UTF-8 and could be lossy for
  invalid byte sequences; and
- an EOF or hangup on the PTY could leave its event reader spinning.

Those are real editor defects and require their own fixes and tests.  They did
not produce the nine TinyCat deletions: the observed HTML was valid UTF-8, and
the exact loss pattern occurs upstream at the WSL relay boundaries.  Fixing
nano-rs byte handling and EOF behavior prevents separate corruption or hangs,
but cannot recover bytes that WSL never delivered.
