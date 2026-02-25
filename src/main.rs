use std::collections::VecDeque;
use std::io::Read;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use clap::Parser;

#[derive(Parser)]
#[command(about = "Run commands sequentially, stopping on first failure")]
struct Args {
    /// Exit code on failure: number or "mirror"
    #[arg(
        short = 'e',
        long = "exit",
        default_value = "1",
        allow_hyphen_values = true
    )]
    exit: String,

    /// Maximum number of output characters to retain on failure
    #[arg(
        short = 'b',
        long = "buffer-size",
        default_value_t = 10_000,
        value_parser = parse_buffer_size
    )]
    buffer_size: usize,

    /// Commands to execute (shell-like quoting supported)
    #[arg(required = true)]
    commands: Vec<String>,
}

enum ExitMode {
    Fixed(i32),
    Mirror,
}

fn parse_buffer_size(s: &str) -> Result<usize, String> {
    let value = s
        .parse::<usize>()
        .map_err(|_| format!("invalid buffer size: {s}"))?;

    if value == 0 {
        return Err("buffer size must be at least 1".to_string());
    }

    Ok(value)
}

impl ExitMode {
    fn parse(s: &str) -> Result<Self, String> {
        if s == "mirror" {
            Ok(ExitMode::Mirror)
        } else {
            s.parse::<i32>()
                .map(ExitMode::Fixed)
                .map_err(|_| format!("invalid exit code: {s} (use an integer or \"mirror\")"))
        }
    }

    fn code(&self, actual: i32) -> i32 {
        match self {
            ExitMode::Fixed(code) => *code,
            ExitMode::Mirror => actual,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

struct Chunk {
    stream: Stream,
    data: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct Fragment {
    stream: Stream,
    data: Vec<u8>,
}

struct CapturedOutput {
    limit: usize,
    head_limit: usize,
    tail_limit: usize,
    total_len: usize,
    truncated: bool,
    head: Vec<Fragment>,
    tail: Vec<Fragment>,
}

impl CapturedOutput {
    fn empty(limit: usize) -> Self {
        let head_limit = limit / 2;
        let tail_limit = limit - head_limit;
        Self {
            limit,
            head_limit,
            tail_limit,
            total_len: 0,
            truncated: false,
            head: Vec::new(),
            tail: Vec::new(),
        }
    }
}

struct OutputBuffer {
    limit: usize,
    head_limit: usize,
    tail_limit: usize,
    total_len: usize,
    head_len: usize,
    tail_len: usize,
    head: Vec<Fragment>,
    tail: VecDeque<Fragment>,
}

impl OutputBuffer {
    fn new(limit: usize) -> Self {
        let head_limit = limit / 2;
        let tail_limit = limit - head_limit;

        Self {
            limit,
            head_limit,
            tail_limit,
            total_len: 0,
            head_len: 0,
            tail_len: 0,
            head: Vec::new(),
            tail: VecDeque::new(),
        }
    }

    fn push(&mut self, stream: Stream, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        self.total_len += data.len();

        let mut remaining = data;
        if self.head_len < self.head_limit {
            let keep = (self.head_limit - self.head_len).min(remaining.len());
            self.push_head(stream, &remaining[..keep]);
            self.head_len += keep;
            remaining = &remaining[keep..];
        }

        if !remaining.is_empty() {
            self.push_tail(stream, remaining);
        }
    }

    fn push_head(&mut self, stream: Stream, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        if let Some(last) = self.head.last_mut()
            && last.stream == stream
        {
            last.data.extend_from_slice(data);
            return;
        }

        self.head.push(Fragment {
            stream,
            data: data.to_vec(),
        });
    }

    fn push_tail(&mut self, stream: Stream, data: &[u8]) {
        if data.is_empty() || self.tail_limit == 0 {
            return;
        }

        if let Some(last) = self.tail.back_mut()
            && last.stream == stream
        {
            last.data.extend_from_slice(data);
            self.tail_len += data.len();
            self.trim_tail();
            return;
        }

        self.tail.push_back(Fragment {
            stream,
            data: data.to_vec(),
        });
        self.tail_len += data.len();
        self.trim_tail();
    }

    fn trim_tail(&mut self) {
        while self.tail_len > self.tail_limit {
            let overflow = self.tail_len - self.tail_limit;
            let mut remove_fragment = false;

            if let Some(front) = self.tail.front_mut() {
                if front.data.len() <= overflow {
                    self.tail_len -= front.data.len();
                    remove_fragment = true;
                } else {
                    front.data.drain(..overflow);
                    self.tail_len -= overflow;
                }
            }

            if remove_fragment {
                self.tail.pop_front();
            }
        }
    }

    fn finish(self) -> CapturedOutput {
        CapturedOutput {
            limit: self.limit,
            head_limit: self.head_limit,
            tail_limit: self.tail_limit,
            total_len: self.total_len,
            truncated: self.total_len > self.limit,
            head: self.head,
            tail: self.tail.into_iter().collect(),
        }
    }
}

#[derive(Debug)]
enum RunError {
    Parse(String),
    Spawn(std::io::Error),
    Read(std::io::Error),
    Wait(std::io::Error),
    ThreadPanic,
    MissingPipe(&'static str),
}

fn read_stream<R: Read + Send + 'static>(
    mut reader: R,
    stream: Stream,
    tx: mpsc::Sender<Chunk>,
) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    let _ = tx.send(Chunk {
                        stream,
                        data: buf[..n].to_vec(),
                    });
                }
                Err(e) => return Err(e),
            }
        }
    })
}

fn status_to_exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }

    1
}

fn write_fragment(stdout: &mut std::io::Stdout, stderr: &mut std::io::Stderr, fragment: &Fragment) {
    match fragment.stream {
        Stream::Stdout => {
            let _ = stdout.write_all(&fragment.data);
        }
        Stream::Stderr => {
            let _ = stderr.write_all(&fragment.data);
        }
    }
}

fn run_command(cmd_str: &str, buffer_size: usize) -> Result<(i32, CapturedOutput), RunError> {
    let parts = shlex::split(cmd_str)
        .ok_or_else(|| RunError::Parse(format!("failed to parse command: {cmd_str}")))?;
    let (program, args) = parts
        .split_first()
        .ok_or_else(|| RunError::Parse("empty command".to_string()))?;

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;

    let stdout = child.stdout.take().ok_or(RunError::MissingPipe("stdout"))?;
    let stderr = child.stderr.take().ok_or(RunError::MissingPipe("stderr"))?;

    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();

    let h1 = read_stream(stdout, Stream::Stdout, tx);
    let h2 = read_stream(stderr, Stream::Stderr, tx2);

    let mut output = OutputBuffer::new(buffer_size);
    for chunk in rx {
        output.push(chunk.stream, &chunk.data);
    }

    h1.join().map_err(|_| RunError::ThreadPanic)??;
    h2.join().map_err(|_| RunError::ThreadPanic)??;

    let status = child.wait().map_err(RunError::Wait)?;
    let exit_code = status_to_exit_code(status);

    if exit_code == 0 {
        return Ok((0, CapturedOutput::empty(buffer_size)));
    }

    Ok((exit_code, output.finish()))
}

fn main() {
    let args = Args::parse();

    let exit_mode = match ExitMode::parse(&args.exit) {
        Ok(mode) => mode,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    for cmd_str in &args.commands {
        let (exit_code, output) = match run_command(cmd_str, args.buffer_size) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("FAILED: {cmd_str}");
                let code = match e {
                    RunError::Parse(msg) => {
                        eprintln!("Error: {msg}");
                        2
                    }
                    RunError::Spawn(err) => {
                        eprintln!("Error: {err}");
                        match err.kind() {
                            std::io::ErrorKind::NotFound => 127,
                            _ => 126,
                        }
                    }
                    RunError::Read(err) => {
                        eprintln!("Error reading child output: {err}");
                        125
                    }
                    RunError::Wait(err) => {
                        eprintln!("Error waiting for child: {err}");
                        125
                    }
                    RunError::ThreadPanic => {
                        eprintln!("Error: output reader thread panicked");
                        125
                    }
                    RunError::MissingPipe(which) => {
                        eprintln!("Error: missing child {which} pipe");
                        125
                    }
                };
                std::process::exit(code);
            }
        };

        if exit_code != 0 {
            eprintln!("FAILED: {cmd_str}");

            let mut stdout = std::io::stdout();
            let mut stderr = std::io::stderr();

            if output.truncated {
                let omitted = output.total_len.saturating_sub(output.limit);
                eprintln!(
                    "Output truncated: omitted {omitted} characters; showing first {} and last {} characters.",
                    output.head_limit, output.tail_limit
                );
            }

            for fragment in &output.head {
                write_fragment(&mut stdout, &mut stderr, fragment);
            }

            if output.truncated {
                let _ = stderr.write_all(b"\n... output in the middle omitted ...\n");
            }

            for fragment in &output.tail {
                write_fragment(&mut stdout, &mut stderr, fragment);
            }

            std::process::exit(exit_mode.code(exit_code));
        }
    }
}

impl From<std::io::Error> for RunError {
    fn from(value: std::io::Error) -> Self {
        RunError::Read(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mirror_exit_mode() {
        assert!(matches!(ExitMode::parse("mirror"), Ok(ExitMode::Mirror)));
    }

    #[test]
    fn parses_integer_exit_mode() {
        assert!(matches!(ExitMode::parse("300"), Ok(ExitMode::Fixed(300))));
        assert!(matches!(ExitMode::parse("-1"), Ok(ExitMode::Fixed(-1))));
    }

    #[test]
    fn rejects_invalid_exit_mode() {
        assert!(ExitMode::parse("not-an-int").is_err());
    }

    #[test]
    fn keeps_complete_output_when_within_limit() {
        let mut output = OutputBuffer::new(10);
        output.push(Stream::Stdout, b"hello");
        output.push(Stream::Stderr, b"!");

        let output = output.finish();
        assert!(!output.truncated);
        assert_eq!(flatten(&output.head, &output.tail), b"hello!");
    }

    #[test]
    fn keeps_start_and_end_when_truncated() {
        let mut output = OutputBuffer::new(10);
        output.push(Stream::Stdout, b"0123456789ABCDE");

        let output = output.finish();
        assert!(output.truncated);
        assert_eq!(flatten(&output.head, &output.tail), b"01234ABCDE");
    }

    #[cfg(unix)]
    #[test]
    fn mirrors_signal_exit_as_128_plus_signal() {
        let (code, _) = run_command("sh -c 'kill -TERM $$'", 10_000).expect("command should run");
        assert_eq!(code, 143);
    }

    fn flatten(head: &[Fragment], tail: &[Fragment]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for fragment in head {
            bytes.extend_from_slice(&fragment.data);
        }
        for fragment in tail {
            bytes.extend_from_slice(&fragment.data);
        }
        bytes
    }
}
