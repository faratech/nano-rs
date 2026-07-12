#[cfg(feature = "libc")]
use std::os::unix::prelude::AsRawFd;
use std::{
    collections::VecDeque,
    io,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

#[cfg(not(feature = "libc"))]
use rustix::fd::{AsFd, AsRawFd};

use signal_hook::low_level::pipe;

use crate::event::timeout::PollTimeout;
use filedescriptor::{poll, pollfd, POLLERR, POLLHUP, POLLIN};

#[cfg(feature = "bracketed-paste")]
use crate::event::sys::unix::parse::{incomplete_paste_bytes, BRACKETED_PASTE_START};
#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource, sys::unix::parse::parse_event, Event, InternalEvent, KeyCode,
};
use crate::terminal::sys::file_descriptor::{tty_fd, FileDesc};

/// Holds a prototypical Waker and a receiver we can wait on when doing select().
#[cfg(feature = "event-stream")]
struct WakePipe {
    receiver: UnixStream,
    waker: Waker,
}

#[cfg(feature = "event-stream")]
impl WakePipe {
    fn new() -> io::Result<Self> {
        let (receiver, sender) = nonblocking_unix_pair()?;
        Ok(WakePipe {
            receiver,
            waker: Waker::new(sender),
        })
    }
}

// I (@zrzka) wasn't able to read more than 1_022 bytes when testing
// reading on macOS/Linux -> we don't need bigger buffer and 1k of bytes
// is enough.
const TTY_BUFFER_SIZE: usize = 1_024;
const ESCAPE_SEQUENCE_TIMEOUT: Duration = Duration::from_millis(100);
const PASTE_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct UnixInternalEventSource {
    parser: Parser,
    tty_buffer: [u8; TTY_BUFFER_SIZE],
    tty: FileDesc<'static>,
    winch_signal_receiver: UnixStream,
    eof_pending: bool,
    #[cfg(feature = "event-stream")]
    wake_pipe: WakePipe,
}

fn nonblocking_unix_pair() -> io::Result<(UnixStream, UnixStream)> {
    let (receiver, sender) = UnixStream::pair()?;
    receiver.set_nonblocking(true)?;
    sender.set_nonblocking(true)?;
    Ok((receiver, sender))
}

impl UnixInternalEventSource {
    pub fn new() -> io::Result<Self> {
        UnixInternalEventSource::from_file_descriptor(tty_fd()?)
    }

    pub(crate) fn from_file_descriptor(input_fd: FileDesc<'static>) -> io::Result<Self> {
        Ok(UnixInternalEventSource {
            parser: Parser::default(),
            tty_buffer: [0u8; TTY_BUFFER_SIZE],
            tty: input_fd,
            eof_pending: false,
            winch_signal_receiver: {
                let (receiver, sender) = nonblocking_unix_pair()?;
                // Unregistering is unnecessary because EventSource is a singleton
                #[cfg(feature = "libc")]
                pipe::register(libc::SIGWINCH, sender)?;
                #[cfg(not(feature = "libc"))]
                pipe::register(rustix::process::Signal::WINCH.as_raw(), sender)?;
                receiver
            },
            #[cfg(feature = "event-stream")]
            wake_pipe: WakePipe::new()?,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOutcome {
    Data(usize),
    WouldBlock,
    Eof,
}

/// Perform one non-blocking read while keeping EOF distinct from EAGAIN.
fn read_nonblocking(fd: &FileDesc, buf: &mut [u8]) -> io::Result<ReadOutcome> {
    loop {
        match fd.read(buf) {
            Ok(0) => return Ok(ReadOutcome::Eof),
            Ok(count) => return Ok(ReadOutcome::Data(count)),
            Err(e) => match e.kind() {
                io::ErrorKind::WouldBlock => return Ok(ReadOutcome::WouldBlock),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(e),
            },
        }
    }
}

impl EventSource for UnixInternalEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        let timeout = PollTimeout::new(timeout);

        fn make_pollfd<F: AsRawFd>(fd: &F) -> pollfd {
            pollfd {
                fd: fd.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            }
        }

        #[cfg(not(feature = "event-stream"))]
        let mut fds = [
            make_pollfd(&self.tty),
            make_pollfd(&self.winch_signal_receiver),
        ];

        #[cfg(feature = "event-stream")]
        let mut fds = [
            make_pollfd(&self.tty),
            make_pollfd(&self.winch_signal_receiver),
            make_pollfd(&self.wake_pipe.receiver),
        ];

        loop {
            // check if there are buffered events from the last read
            if let Some(event) = self.parser.next() {
                return Ok(Some(event));
            }
            if let Some(event) = self.parser.flush_expired_event() {
                return Ok(Some(event));
            }
            if self.eof_pending {
                return Err(terminal_eof());
            }
            if timeout.elapsed() {
                return Ok(None);
            }

            let poll_timeout = self.parser.limit_timeout(timeout.leftover());
            match poll(&mut fds, poll_timeout) {
                Err(filedescriptor::Error::Poll(e)) | Err(filedescriptor::Error::Io(e)) => {
                    match e.kind() {
                        // retry on EINTR
                        io::ErrorKind::Interrupted => continue,
                        _ => return Err(e),
                    }
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("got unexpected error while polling: {:?}", e),
                    ))
                }
                Ok(_) => (),
            };
            if fds[0].revents & POLLIN != 0 {
                loop {
                    let available = self.tty.bytes_available()?;
                    if available == 0 {
                        break;
                    }
                    let read_len = available.min(TTY_BUFFER_SIZE);
                    match read_nonblocking(&self.tty, &mut self.tty_buffer[..read_len])? {
                        ReadOutcome::Data(read_count) => {
                            self.parser
                                .advance(&self.tty_buffer[..read_count], available > read_count);
                        }
                        ReadOutcome::WouldBlock => break,
                        ReadOutcome::Eof => {
                            return self.finish_at_eof();
                        }
                    }
                }
                if let Some(event) = self.parser.next() {
                    return Ok(Some(event));
                }
            }
            if fds[0].revents & POLLHUP != 0 {
                return self.finish_at_eof();
            }
            if fds[0].revents & POLLERR != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "terminal input reported a poll error",
                ));
            }
            if fds[1].revents & POLLIN != 0 {
                #[cfg(feature = "libc")]
                let fd = FileDesc::new(self.winch_signal_receiver.as_raw_fd(), false);
                #[cfg(not(feature = "libc"))]
                let fd = FileDesc::Borrowed(self.winch_signal_receiver.as_fd());
                // drain the pipe
                while let ReadOutcome::Data(_) = read_nonblocking(&fd, &mut [0; 1024])? {}
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

            #[cfg(feature = "event-stream")]
            if fds[2].revents & POLLIN != 0 {
                #[cfg(feature = "libc")]
                let fd = FileDesc::new(self.wake_pipe.receiver.as_raw_fd(), false);
                #[cfg(not(feature = "libc"))]
                let fd = FileDesc::Borrowed(self.wake_pipe.receiver.as_fd());
                // drain the pipe
                while let ReadOutcome::Data(_) = read_nonblocking(&fd, &mut [0; 1024])? {}

                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Poll operation was woken up by `Waker::wake`",
                ));
            }

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
        self.wake_pipe.waker.clone()
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
    #[cfg(feature = "libc")]
    use std::os::fd::IntoRawFd;
    #[cfg(feature = "bracketed-paste")]
    use std::thread;

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
    fn nonblocking_read_distinguishes_would_block_from_eof() {
        let (reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let reader = file_desc(reader);
        let mut buffer = [0; 8];

        assert_eq!(
            read_nonblocking(&reader, &mut buffer).unwrap(),
            ReadOutcome::WouldBlock
        );

        drop(writer);
        assert_eq!(
            read_nonblocking(&reader, &mut buffer).unwrap(),
            ReadOutcome::Eof
        );
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
    fn complete_paste_is_returned_before_hangup() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut source = UnixInternalEventSource::from_file_descriptor(file_desc(reader)).unwrap();
        let payload = [b'a', 0xff, 0xfe, b'\n', b'z'];

        writer.write_all(b"\x1B[200~").unwrap();
        writer.write_all(&payload).unwrap();
        writer.write_all(b"\x1B[201~").unwrap();
        drop(writer);

        assert_eq!(
            source.try_read(Some(Duration::from_secs(1))).unwrap(),
            Some(InternalEvent::Event(Event::PasteBytes(payload.to_vec())))
        );

        let error = source
            .try_read(Some(Duration::from_millis(100)))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(feature = "bracketed-paste")]
    #[test]
    fn parser_keeps_byte_fragmented_paste_markers_and_raw_payload() {
        let payload = [b'a', 0xff, 0xfe, b'z'];
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
    fn event_source_reads_complete_paste_from_delayed_single_byte_writes() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
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
