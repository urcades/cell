use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::marker::PhantomData;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboundFrame<Request> {
    Message(Request),
    ProtocolError { raw: String, error: String },
    StreamClosed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OutboundMessage<Response, Event> {
    Response(Response),
    Event(Event),
}

pub struct JsonLineReader<R, Message> {
    reader: BufReader<R>,
    line: String,
    stream_closed: bool,
    marker: PhantomData<Message>,
}

impl<R, Message> JsonLineReader<R, Message>
where
    R: Read,
    Message: DeserializeOwned,
{
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: String::new(),
            stream_closed: false,
            marker: PhantomData,
        }
    }

    pub fn read_frame(&mut self) -> InboundFrame<Message> {
        if self.stream_closed {
            return InboundFrame::StreamClosed;
        }

        loop {
            self.line.clear();
            match self.reader.read_line(&mut self.line) {
                Ok(0) => {
                    self.stream_closed = true;
                    return InboundFrame::StreamClosed;
                }
                Ok(_) if self.line.trim().is_empty() => continue,
                Ok(_) => {
                    return match serde_json::from_str::<Message>(&self.line) {
                        Ok(message) => InboundFrame::Message(message),
                        Err(error) => InboundFrame::ProtocolError {
                            raw: self.line.trim_end_matches(['\r', '\n']).to_string(),
                            error: error.to_string(),
                        },
                    };
                }
                Err(error) => {
                    self.stream_closed = true;
                    return InboundFrame::ProtocolError {
                        raw: String::new(),
                        error: error.to_string(),
                    };
                }
            }
        }
    }

    pub fn into_inner(self) -> R {
        self.reader.into_inner()
    }
}

pub struct JsonLineWriter<W, Message> {
    writer: W,
    marker: PhantomData<Message>,
}

impl<W, Message> JsonLineWriter<W, Message>
where
    W: Write,
    Message: Serialize,
{
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            marker: PhantomData,
        }
    }

    pub fn send(&mut self, message: &Message) -> Result<(), String> {
        serde_json::to_writer(&mut self.writer, message).map_err(|error| error.to_string())?;
        self.writer
            .write_all(b"\n")
            .map_err(|error| error.to_string())?;
        self.writer.flush().map_err(|error| error.to_string())
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

pub struct JsonLineClient<R, W, Outbound, Inbound> {
    reader: JsonLineReader<R, Inbound>,
    writer: JsonLineWriter<W, Outbound>,
}

impl<R, W, Outbound, Inbound> JsonLineClient<R, W, Outbound, Inbound>
where
    R: Read,
    W: Write,
    Outbound: Serialize,
    Inbound: DeserializeOwned,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: JsonLineReader::new(reader),
            writer: JsonLineWriter::new(writer),
        }
    }

    pub fn send(&mut self, message: &Outbound) -> Result<(), String> {
        self.writer.send(message)
    }

    pub fn read_frame(&mut self) -> InboundFrame<Inbound> {
        self.reader.read_frame()
    }

    pub fn into_parts(self) -> (JsonLineReader<R, Inbound>, JsonLineWriter<W, Outbound>) {
        (self.reader, self.writer)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClientReceiveError {
    Timeout,
    Disconnected,
}

pub struct JsonLineClientSession<W, Outbound, Inbound> {
    writer: JsonLineWriter<W, Outbound>,
    inbox: Receiver<InboundFrame<Inbound>>,
    pending_frames: VecDeque<InboundFrame<Inbound>>,
}

impl<W, Outbound, Inbound> JsonLineClientSession<W, Outbound, Inbound>
where
    W: Write,
    Outbound: Serialize,
    Inbound: DeserializeOwned + Send + 'static,
{
    pub fn new(reader: impl Read + Send + 'static, writer: W) -> Self {
        let (tx, rx) = mpsc::channel();
        let _reader_thread = spawn_reader_thread(reader, tx);
        Self {
            writer: JsonLineWriter::new(writer),
            inbox: rx,
            pending_frames: VecDeque::new(),
        }
    }

    pub fn send(&mut self, message: &Outbound) -> Result<(), String> {
        self.writer.send(message)
    }

    pub fn recv_frame(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<InboundFrame<Inbound>, ClientReceiveError> {
        if let Some(frame) = self.pending_frames.pop_front() {
            return Ok(frame);
        }

        match timeout {
            Some(duration) => self
                .inbox
                .recv_timeout(duration)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => ClientReceiveError::Timeout,
                    mpsc::RecvTimeoutError::Disconnected => ClientReceiveError::Disconnected,
                }),
            None => self.inbox.recv().map_err(|_| ClientReceiveError::Disconnected),
        }
    }

    pub fn push_back(&mut self, frame: InboundFrame<Inbound>) {
        self.pending_frames.push_back(frame);
    }

    pub fn into_inner(self) -> W {
        self.writer.into_inner()
    }
}

pub struct TransportEmitter<Response, Event> {
    sender: Sender<OutboundMessage<Response, Event>>,
}

impl<Response, Event> Clone for TransportEmitter<Response, Event> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<Response, Event> TransportEmitter<Response, Event> {
    pub fn new(sender: Sender<OutboundMessage<Response, Event>>) -> Self {
        Self { sender }
    }

    pub fn send_response(&self, response: Response) -> Result<(), String> {
        self.sender
            .send(OutboundMessage::Response(response))
            .map_err(|error| error.to_string())
    }

    pub fn send_event(&self, event: Event) -> Result<(), String> {
        self.sender
            .send(OutboundMessage::Event(event))
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceControl {
    Continue,
    Exit(i32),
}

pub trait JsonLineService {
    type Request: DeserializeOwned + Send + 'static;
    type Response: Serialize + Send + 'static;
    type Event: Serialize + Send + 'static;

    fn handle_frame(
        &mut self,
        frame: InboundFrame<Self::Request>,
        emitter: &TransportEmitter<Self::Response, Self::Event>,
    ) -> Result<ServiceControl, String>;

    fn tick(
        &mut self,
        _emitter: &TransportEmitter<Self::Response, Self::Event>,
    ) -> Result<ServiceControl, String> {
        Ok(ServiceControl::Continue)
    }
}

pub fn run_stdio_service<S>(
    reader: impl Read + Send + 'static,
    mut writer: impl Write,
    mut service: S,
) -> Result<i32, String>
where
    S: JsonLineService,
{
    let (inbound_tx, inbound_rx) = mpsc::channel::<InboundFrame<S::Request>>();
    let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundMessage<S::Response, S::Event>>();
    let emitter = TransportEmitter::new(outbound_tx.clone());
    let mut stream_closed_delivered = false;

    let _reader_thread = spawn_reader_thread(reader, inbound_tx);

    loop {
        drain_outbound_messages(&mut writer, &outbound_rx)?;

        match service.tick(&emitter)? {
            ServiceControl::Continue => {}
            ServiceControl::Exit(code) => {
                drain_outbound_messages(&mut writer, &outbound_rx)?;
                return Ok(code);
            }
        }

        match inbound_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(frame) => {
                if matches!(frame, InboundFrame::StreamClosed) {
                    stream_closed_delivered = true;
                }
                match service.handle_frame(frame, &emitter)? {
                    ServiceControl::Continue => {}
                    ServiceControl::Exit(code) => {
                        drain_outbound_messages(&mut writer, &outbound_rx)?;
                        return Ok(code);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if stream_closed_delivered {
                    continue;
                }
                stream_closed_delivered = true;
                match service.handle_frame(InboundFrame::StreamClosed, &emitter)? {
                    ServiceControl::Continue => continue,
                    ServiceControl::Exit(code) => {
                        drain_outbound_messages(&mut writer, &outbound_rx)?;
                        return Ok(code);
                    }
                }
            }
        }
    }
}

fn spawn_reader_thread<Request>(
    reader: impl Read + Send + 'static,
    inbound_tx: Sender<InboundFrame<Request>>,
) -> thread::JoinHandle<()>
where
    Request: DeserializeOwned + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = JsonLineReader::new(reader);
        loop {
            let frame = reader.read_frame();
            let stream_closed = matches!(frame, InboundFrame::StreamClosed);
            if inbound_tx.send(frame).is_err() {
                break;
            }
            if stream_closed {
                break;
            }
        }
    })
}

fn drain_outbound_messages<Response, Event>(
    writer: &mut impl Write,
    outbound_rx: &Receiver<OutboundMessage<Response, Event>>,
) -> Result<(), String>
where
    Response: Serialize,
    Event: Serialize,
{
    while let Ok(message) = outbound_rx.try_recv() {
        match message {
            OutboundMessage::Response(response) => write_json_line(writer, &response)?,
            OutboundMessage::Event(event) => write_json_line(writer, &event)?,
        }
    }
    Ok(())
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value).map_err(|error| error.to_string())?;
    writer.write_all(b"\n").map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TestRequest {
        kind: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TestResponse {
        kind: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct TestEvent {
        kind: String,
    }

    #[test]
    fn typed_reader_reports_messages_protocol_errors_and_close() {
        let input = std::io::Cursor::new("{\"kind\":\"hello\"}\nnot-json\n");
        let mut reader = JsonLineReader::<_, TestRequest>::new(input);

        assert_eq!(
            reader.read_frame(),
            InboundFrame::Message(TestRequest {
                kind: "hello".to_string(),
            })
        );
        assert!(
            matches!(reader.read_frame(), InboundFrame::ProtocolError { raw, .. } if raw == "not-json")
        );
        assert_eq!(reader.read_frame(), InboundFrame::StreamClosed);
    }

    #[test]
    fn typed_writer_serializes_and_flushes_messages() {
        let mut writer = JsonLineWriter::<_, TestResponse>::new(Vec::new());
        writer
            .send(&TestResponse {
                kind: "hello".to_string(),
            })
            .expect("send message");
        writer
            .send(&TestResponse {
                kind: "goodbye".to_string(),
            })
            .expect("send message");

        let output = String::from_utf8(writer.into_inner()).expect("utf8");
        assert_eq!(output, "{\"kind\":\"hello\"}\n{\"kind\":\"goodbye\"}\n");
    }

    #[test]
    fn typed_client_round_trips_reader_and_writer_halves() {
        let input = std::io::Cursor::new("{\"kind\":\"event\"}\n");
        let output = Vec::new();
        let mut client = JsonLineClient::<_, _, TestRequest, TestEvent>::new(input, output);

        client
            .send(&TestRequest {
                kind: "hello".to_string(),
            })
            .expect("send request");
        assert_eq!(
            client.read_frame(),
            InboundFrame::Message(TestEvent {
                kind: "event".to_string(),
            })
        );

        let (_reader, writer) = client.into_parts();
        let output = String::from_utf8(writer.into_inner()).expect("utf8");
        assert_eq!(output, "{\"kind\":\"hello\"}\n");
    }

    struct RecordingService {
        frames: Arc<Mutex<Vec<String>>>,
        closed: bool,
    }

    impl JsonLineService for RecordingService {
        type Request = TestRequest;
        type Response = TestResponse;
        type Event = TestEvent;

        fn handle_frame(
            &mut self,
            frame: InboundFrame<Self::Request>,
            emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            match frame {
                InboundFrame::Message(request) => {
                    self.frames
                        .lock()
                        .expect("frames lock")
                        .push(format!("message:{}", request.kind));
                    emitter.send_response(TestResponse { kind: request.kind })?;
                }
                InboundFrame::ProtocolError { raw, error } => {
                    self.frames
                        .lock()
                        .expect("frames lock")
                        .push(format!("protocol:{raw}:{error}"));
                }
                InboundFrame::StreamClosed => {
                    self.frames
                        .lock()
                        .expect("frames lock")
                        .push("closed".to_string());
                    self.closed = true;
                }
            }
            Ok(ServiceControl::Continue)
        }

        fn tick(
            &mut self,
            _emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            if self.closed {
                Ok(ServiceControl::Exit(0))
            } else {
                Ok(ServiceControl::Continue)
            }
        }
    }

    struct OrderedService {
        finished: bool,
    }

    impl JsonLineService for OrderedService {
        type Request = TestRequest;
        type Response = TestResponse;
        type Event = TestEvent;

        fn handle_frame(
            &mut self,
            frame: InboundFrame<Self::Request>,
            emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            match frame {
                InboundFrame::Message(request) => {
                    emitter.send_response(TestResponse {
                        kind: format!("response:{}", request.kind),
                    })?;
                    emitter.send_event(TestEvent {
                        kind: format!("event:{}", request.kind),
                    })?;
                    emitter.send_response(TestResponse {
                        kind: format!("final:{}", request.kind),
                    })?;
                }
                InboundFrame::StreamClosed => self.finished = true,
                InboundFrame::ProtocolError { .. } => {}
            }
            Ok(ServiceControl::Continue)
        }

        fn tick(
            &mut self,
            _emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            if self.finished {
                Ok(ServiceControl::Exit(0))
            } else {
                Ok(ServiceControl::Continue)
            }
        }
    }

    struct BackgroundEmitterService {
        closed: bool,
        worker_started: bool,
        worker_done: Arc<AtomicBool>,
    }

    impl JsonLineService for BackgroundEmitterService {
        type Request = TestRequest;
        type Response = TestResponse;
        type Event = TestEvent;

        fn handle_frame(
            &mut self,
            frame: InboundFrame<Self::Request>,
            emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            match frame {
                InboundFrame::Message(request) => {
                    if !self.worker_started {
                        self.worker_started = true;
                        let emitter = emitter.clone();
                        let done = Arc::clone(&self.worker_done);
                        thread::spawn(move || {
                            emitter
                                .send_event(TestEvent {
                                    kind: format!("event:{}", request.kind),
                                })
                                .expect("send event");
                            emitter
                                .send_response(TestResponse {
                                    kind: format!("response:{}", request.kind),
                                })
                                .expect("send response");
                            done.store(true, Ordering::SeqCst);
                        });
                    }
                }
                InboundFrame::StreamClosed => self.closed = true,
                InboundFrame::ProtocolError { .. } => {}
            }
            Ok(ServiceControl::Continue)
        }

        fn tick(
            &mut self,
            _emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            if self.closed && self.worker_done.load(Ordering::SeqCst) {
                Ok(ServiceControl::Exit(0))
            } else {
                Ok(ServiceControl::Continue)
            }
        }
    }

    #[test]
    fn reports_protocol_errors_and_stream_close() {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let service = RecordingService {
            frames: Arc::clone(&frames),
            closed: false,
        };
        let input = std::io::Cursor::new("{\"kind\":\"hello\"}\nnot-json\n");
        let mut output = Vec::new();

        let exit_code = run_stdio_service(input, &mut output, service).expect("transport run");
        assert_eq!(exit_code, 0);

        let frames = frames.lock().expect("frames lock");
        assert!(frames.iter().any(|entry| entry == "message:hello"));
        assert!(
            frames
                .iter()
                .any(|entry| entry.starts_with("protocol:not-json:"))
        );
        assert!(frames.iter().any(|entry| entry == "closed"));
    }

    #[test]
    fn preserves_response_and_event_order() {
        let input = std::io::Cursor::new("{\"kind\":\"hello\"}\n");
        let mut output = Vec::new();

        let exit_code = run_stdio_service(input, &mut output, OrderedService { finished: false })
            .expect("transport run");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines[0]["kind"], "response:hello");
        assert_eq!(lines[1]["kind"], "event:hello");
        assert_eq!(lines[2]["kind"], "final:hello");
    }

    #[test]
    fn supports_background_emitters_from_other_threads() {
        let input = std::io::Cursor::new("{\"kind\":\"hello\"}\n");
        let mut output = Vec::new();

        let exit_code = run_stdio_service(
            input,
            &mut output,
            BackgroundEmitterService {
                closed: false,
                worker_started: false,
                worker_done: Arc::new(AtomicBool::new(false)),
            },
        )
        .expect("transport run");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines[0]["kind"], "event:hello");
        assert_eq!(lines[1]["kind"], "response:hello");
    }

    struct SingleCloseService {
        close_count: Arc<Mutex<usize>>,
        idle_ticks_after_close: usize,
    }

    impl JsonLineService for SingleCloseService {
        type Request = TestRequest;
        type Response = TestResponse;
        type Event = TestEvent;

        fn handle_frame(
            &mut self,
            frame: InboundFrame<Self::Request>,
            _emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            if matches!(frame, InboundFrame::StreamClosed) {
                *self.close_count.lock().expect("close count lock") += 1;
            }
            Ok(ServiceControl::Continue)
        }

        fn tick(
            &mut self,
            _emitter: &TransportEmitter<Self::Response, Self::Event>,
        ) -> Result<ServiceControl, String> {
            if *self.close_count.lock().expect("close count lock") > 0 {
                self.idle_ticks_after_close += 1;
                if self.idle_ticks_after_close >= 3 {
                    return Ok(ServiceControl::Exit(0));
                }
            }
            Ok(ServiceControl::Continue)
        }
    }

    #[test]
    fn delivers_stream_closed_only_once() {
        let input = std::io::Cursor::new("{\"kind\":\"hello\"}\n");
        let mut output = Vec::new();
        let close_count = Arc::new(Mutex::new(0usize));

        let exit_code = run_stdio_service(
            input,
            &mut output,
            SingleCloseService {
                close_count: Arc::clone(&close_count),
                idle_ticks_after_close: 0,
            },
        )
        .expect("transport run");
        assert_eq!(exit_code, 0);
        assert_eq!(*close_count.lock().expect("close count lock"), 1);
    }
}
