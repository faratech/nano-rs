use std::{
    collections::VecDeque,
    io,
    time::{Duration, Instant},
};

use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use signal_hook_mio::v1_0::Signals;

#[cfg(feature = "bracketed-paste")]
use crate::event::sys::unix::parse::{incomplete_paste_bytes, BRACKETED_PASTE_START};
#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource, sys::unix::parse::parse_event, timeout::PollTimeout, Event, InternalEvent,
    KeyCode,
};
use crate::terminal::sys::file_descriptor::{tty_fd, FileDesc};

// Tokens to identify file descriptor
const TTY_TOKEN: Token = Token(0);
const SIGNAL_TOKEN: Token = Token(1);
#[cfg(feature = "event-stream")]
const WAKE_TOKEN: Token = Token(2);

// I (@zrzka) wasn't able to read more than 1_022 bytes when testing
// reading on macOS/Linux -> we don't need bigger buffer and 1k of bytes
// is enough.
const TTY_BUFFER_SIZE: usize = 1_024;
const ESCAPE_SEQUENCE_TIMEOUT: Duration = Duration::from_millis(100);
const PASTE_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct UnixInternalEventSource {
    poll: Poll,
    events: Events,
    parser: Parser,
    tty_buffer: [u8; TTY_BUFFER_SIZE],
    tty_fd: FileDesc<'static>,
    signals: Signals,
    eof_pending: bool,
    #[cfg(feature = "event-stream")]
    waker: Waker,
}

impl UnixInternalEventSource {
    pub fn new() -> io::Result<Self> {
        UnixInternalEventSource::from_file_descriptor(tty_fd()?)
    }

    pub(crate) fn from_file_descriptor(input_fd: FileDesc<'static>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let registry = poll.registry();

        let tty_raw_fd = input_fd.raw_fd();
        let mut tty_ev = SourceFd(&tty_raw_fd);
        registry.register(&mut tty_ev, TTY_TOKEN, Interest::READABLE)?;

        let mut signals = Signals::new([signal_hook::consts::SIGWINCH])?;
        registry.register(&mut signals, SIGNAL_TOKEN, Interest::READABLE)?;

        #[cfg(feature = "event-stream")]
        let waker = Waker::new(registry, WAKE_TOKEN)?;

        Ok(UnixInternalEventSource {
            poll,
            events: Events::with_capacity(3),
            parser: Parser::default(),
            tty_buffer: [0u8; TTY_BUFFER_SIZE],
            tty_fd: input_fd,
            signals,
            eof_pending: false,
            #[cfg(feature = "event-stream")]
            waker,
        })
    }

    fn finish_at_eof(&mut self) -> io::Result<Option<InternalEvent>> {
        let event = self
            .parser
            .finish_incomplete_paste()
            .or_else(|| self.parser.next());
        if let Some(event) = event {
            self.eof_pending = true;
            Ok(Some(event))
        } else {
            Err(terminal_eof())
        }
    }
}

fn terminal_eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "terminal input reached EOF")
}

impl EventSource for UnixInternalEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        if let Some(event) = self.parser.next() {
            return Ok(Some(event));
        }
        if let Some(event) = self.parser.flush_expired_event() {
            return Ok(Some(event));
        }
        if self.eof_pending {
            return Err(terminal_eof());
        }

        let timeout = PollTimeout::new(timeout);

        loop {
            let poll_timeout = self.parser.limit_timeout(timeout.leftover());
            if let Err(e) = self.poll.poll(&mut self.events, poll_timeout) {
                // Mio will throw an interrupted error in case of cursor position retrieval. We need to retry until it succeeds.
                // Previous versions of Mio (< 0.7) would automatically retry the poll call if it was interrupted (if EINTR was returned).
                // https://docs.rs/mio/0.7.0/mio/struct.Poll.html#notes
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                } else {
                    return Err(e);
                }
            };

            if self.events.is_empty() {
                if let Some(event) = self.parser.flush_expired_event() {
                    return Ok(Some(event));
                }
                if timeout.elapsed() {
                    return Ok(None);
                }
                continue;
            }

            let mut readiness = [None; 3];
            for (slot, event) in readiness.iter_mut().zip(self.events.iter()) {
                *slot = Some((event.token(), event.is_read_closed(), event.is_error()));
            }
            for (token, read_closed, read_error) in readiness.into_iter().flatten() {
                match token {
                    TTY_TOKEN => {
                        loop {
                            let available = self.tty_fd.bytes_available()?;
                            if available == 0 {
                                break;
                            }
                            let read_len = available.min(TTY_BUFFER_SIZE);
                            match self.tty_fd.read(&mut self.tty_buffer[..read_len]) {
                                Ok(0) => {
                                    return self.finish_at_eof();
                                }
                                Ok(read_count) => {
                                    self.parser.advance(
                                        &self.tty_buffer[..read_count],
                                        available > read_count,
                                    );
                                }
                                Err(e) => {
                                    // No more data to read at the moment. We will receive another event
                                    if e.kind() == io::ErrorKind::WouldBlock {
                                        break;
                                    }
                                    // once more data is available to read.
                                    else if e.kind() == io::ErrorKind::Interrupted {
                                        continue;
                                    } else {
                                        return Err(e);
                                    }
                                }
                            };
                        }
                        if read_closed {
                            return self.finish_at_eof();
                        }
                        if read_error {
                            return Err(io::Error::new(
                                io::ErrorKind::Other,
                                "terminal input reported a poll error",
                            ));
                        }
                        if let Some(event) = self.parser.next() {
                            return Ok(Some(event));
                        }
                    }
                    SIGNAL_TOKEN => {
                        if self.signals.pending().next() == Some(signal_hook::consts::SIGWINCH) {
                            // TODO Should we remove tput?
                            //
                            // This can take a really long time, because terminal::size can
                            // launch new process (tput) and then it parses its output. It's
                            // not a really long time from the absolute time point of view, but
                            // it's a really long time from the mio, async-std/tokio executor, ...
                            // point of view.
                            let new_size = crate::terminal::size()?;
                            return Ok(Some(InternalEvent::Event(Event::Resize(
                                new_size.0, new_size.1,
                            ))));
                        }
                    }
                    #[cfg(feature = "event-stream")]
                    WAKE_TOKEN => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Poll operation was woken up by `Waker::wake`",
                        ));
                    }
                    _ => unreachable!("Synchronize Evented handle registration & token handling"),
                }
            }

            // Processing above can take some time, check if timeout expired
            if let Some(event) = self.parser.flush_expired_event() {
                return Ok(Some(event));
            }
            if timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.waker.clone()
    }
}

//
// Following `Parser` structure exists for two reasons:
//
//  * mimic anes Parser interface
//  * move the advancing, parsing, ... stuff out of the `try_read` method
//
#[derive(Debug)]
struct Parser {
    buffer: Vec<u8>,
    internal_events: VecDeque<InternalEvent>,
    ambiguous_escape_since: Option<Instant>,
    paste_idle_since: Option<Instant>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            // This buffer is used for -> 1 <- ANSI escape sequence. Are we
            // aware of any ANSI escape sequence that is bigger? Can we make
            // it smaller?
            //
            // Probably not worth spending more time on this as "there's a plan"
            // to use the anes crate parser.
            buffer: Vec::with_capacity(256),
            // TTY_BUFFER_SIZE is 1_024 bytes. How many ANSI escape sequences can
            // fit? What is an average sequence length? Let's guess here
            // and say that the average ANSI escape sequence length is 8 bytes. Thus
            // the buffer size should be 1024/8=128 to avoid additional allocations
            // when processing large amounts of data.
            //
            // There's no need to make it bigger, because when you look at the `try_read`
            // method implementation, all events are consumed before the next TTY_BUFFER
            // is processed -> events pushed.
            internal_events: VecDeque::with_capacity(128),
            ambiguous_escape_since: None,
            paste_idle_since: None,
        }
    }
}

impl Parser {
    fn advance(&mut self, buffer: &[u8], more: bool) {
        let received_at = Instant::now();
        for (idx, byte) in buffer.iter().enumerate() {
            let more = idx + 1 < buffer.len() || more;

            self.buffer.push(*byte);

            // A short PTY read is not an event boundary.  Keep ambiguous ESC/CSI
            // prefixes for a bounded idle interval so escape sequences (including
            // bracketed-paste markers) can span arbitrary kernel reads.
            let starts_with_escape = self.buffer.first() == Some(&b'\x1B');
            let input_available = more || starts_with_escape;

            match parse_event(&self.buffer, input_available) {
                Ok(Some(ie)) => {
                    self.internal_events.push_back(ie);
                    self.buffer.clear();
                    self.ambiguous_escape_since = None;
                    self.paste_idle_since = None;
                }
                Ok(None) => {
                    // Event can't be parsed, because we don't have enough bytes for
                    // the current sequence. Keep the buffer and process next bytes.
                    if self.paste_is_in_progress() {
                        self.ambiguous_escape_since = None;
                        self.paste_idle_since = Some(received_at);
                    } else if starts_with_escape {
                        self.ambiguous_escape_since = Some(Instant::now());
                        self.paste_idle_since = None;
                    } else {
                        self.ambiguous_escape_since = None;
                        self.paste_idle_since = None;
                    }
                }
                Err(_) => {
                    // Event can't be parsed (not enough parameters, parameter is not a number, ...).
                    // Clear the buffer and continue with another sequence.
                    self.buffer.clear();
                    self.ambiguous_escape_since = None;
                    self.paste_idle_since = None;
                }
            }
        }
    }

    fn paste_is_in_progress(&self) -> bool {
        #[cfg(feature = "bracketed-paste")]
        {
            self.buffer.starts_with(BRACKETED_PASTE_START)
        }
        #[cfg(not(feature = "bracketed-paste"))]
        {
            false
        }
    }

    fn limit_timeout(&self, requested: Option<Duration>) -> Option<Duration> {
        let mut limited = requested;
        if let Some(since) = self.ambiguous_escape_since {
            let left = ESCAPE_SEQUENCE_TIMEOUT.saturating_sub(since.elapsed());
            limited = Some(limited.map_or(left, |duration| duration.min(left)));
        }
        if let Some(since) = self.paste_idle_since {
            let left = PASTE_IDLE_TIMEOUT.saturating_sub(since.elapsed());
            limited = Some(limited.map_or(left, |duration| duration.min(left)));
        }
        limited
    }

    fn flush_expired_event(&mut self) -> Option<InternalEvent> {
        if matches!(
            self.paste_idle_since,
            Some(since) if since.elapsed() >= PASTE_IDLE_TIMEOUT
        ) {
            return self.finish_incomplete_paste();
        }

        let since = self.ambiguous_escape_since?;
        if since.elapsed() < ESCAPE_SEQUENCE_TIMEOUT {
            return None;
        }

        self.ambiguous_escape_since = None;
        let remainder = self.buffer.get(1..).unwrap_or_default().to_vec();
        self.buffer.clear();
        self.internal_events
            .push_back(InternalEvent::Event(Event::Key(KeyCode::Esc.into())));
        if !remainder.is_empty() {
            self.advance(&remainder, false);
        }
        self.next()
    }

    fn finish_incomplete_paste(&mut self) -> Option<InternalEvent> {
        #[cfg(feature = "bracketed-paste")]
        let paste = incomplete_paste_bytes(&self.buffer)?;
        #[cfg(not(feature = "bracketed-paste"))]
        return None;

        #[cfg(feature = "bracketed-paste")]
        {
            self.buffer.clear();
            self.ambiguous_escape_since = None;
            self.paste_idle_since = None;
            self.internal_events
                .push_back(InternalEvent::Event(Event::PasteBytesIncomplete(paste)));
            self.next()
        }
    }
}

impl Iterator for Parser {
    type Item = InternalEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.internal_events.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "bracketed-paste")]
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::thread;

    #[cfg(feature = "libc")]
    use std::os::fd::IntoRawFd;

    fn file_desc(stream: UnixStream) -> FileDesc<'static> {
        #[cfg(feature = "libc")]
        {
            FileDesc::new(stream.into_raw_fd(), true)
        }

        #[cfg(not(feature = "libc"))]
        {
            FileDesc::Owned(stream.into())
        }
    }

    #[test]
    fn closed_input_returns_unexpected_eof() {
        let (reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();

        let mut source = UnixInternalEventSource::from_file_descriptor(file_desc(reader)).unwrap();
        drop(writer);

        let error = source
            .try_read(Some(Duration::from_millis(100)))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn parser_keeps_byte_fragmented_paste_markers_and_raw_payload() {
        let payload = [b'a', 0xff, 0xfe, b'\n', b'z'];
        let mut input = b"\x1B[200~".to_vec();
        input.extend_from_slice(&payload);
        input.extend_from_slice(b"\x1B[201~");

        let mut parser = Parser::default();
        for (index, byte) in input.iter().enumerate() {
            parser.advance(&[*byte], false);
            if index + 1 < input.len() {
                assert!(parser.next().is_none(), "event emitted after byte {index}");
            }
        }

        assert_eq!(
            parser.next(),
            Some(InternalEvent::Event(Event::PasteBytes(payload.to_vec())))
        );
        assert!(parser.next().is_none());
    }

    #[test]
    fn standalone_escape_is_emitted_after_ambiguity_timeout() {
        let mut parser = Parser::default();
        parser.advance(b"\x1B", false);
        assert!(parser.next().is_none());

        parser.ambiguous_escape_since = Some(Instant::now() - ESCAPE_SEQUENCE_TIMEOUT);
        assert_eq!(
            parser.flush_expired_event(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Esc.into())))
        );
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn paste_idle_timeout_emits_incomplete_bytes() {
        let mut parser = Parser::default();
        parser.advance(b"\x1B[200~payload", false);
        parser.paste_idle_since = Some(Instant::now() - PASTE_IDLE_TIMEOUT);

        assert_eq!(
            parser.flush_expired_event(),
            Some(InternalEvent::Event(Event::PasteBytesIncomplete(
                b"payload".to_vec()
            )))
        );
        assert!(parser.buffer.is_empty());
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn incomplete_paste_strips_longest_partial_closing_suffix() {
        let mut parser = Parser::default();
        parser.advance(b"\x1B[200~payload\x1B[20", false);

        assert_eq!(
            parser.finish_incomplete_paste(),
            Some(InternalEvent::Event(Event::PasteBytesIncomplete(
                b"payload".to_vec()
            )))
        );
        assert!(parser.buffer.is_empty());
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn blocking_stream_does_not_read_after_available_bytes_are_drained() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(file_desc(reader)).unwrap();
        writer.write_all(b"\x1B[200~still arriving").unwrap();

        let started = Instant::now();
        assert_eq!(
            source.try_read(Some(Duration::from_millis(30))).unwrap(),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn eof_emits_incomplete_paste_before_eof_error() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(file_desc(reader)).unwrap();
        writer.write_all(b"\x1B[200~payload\x1B[201").unwrap();
        drop(writer);

        assert_eq!(
            source.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(InternalEvent::Event(Event::PasteBytesIncomplete(
                b"payload".to_vec()
            )))
        );
        assert_eq!(
            source
                .try_read(Some(Duration::from_millis(30)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn event_source_reads_markers_from_delayed_single_byte_writes() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(file_desc(reader)).unwrap();
        let payload = [b'x', 0xff, 0xfe, b'y'];
        let mut input = b"\x1B[200~".to_vec();
        input.extend_from_slice(&payload);
        input.extend_from_slice(b"\x1B[201~");

        let sender = thread::spawn(move || {
            for byte in input {
                writer.write_all(&[byte]).unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        });

        assert_eq!(
            source.try_read(Some(Duration::from_secs(2))).unwrap(),
            Some(InternalEvent::Event(Event::PasteBytes(payload.to_vec())))
        );
        sender.join().unwrap();
    }
}
