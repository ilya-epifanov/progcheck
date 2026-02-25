# `progcheck` - progressive failure checking tool

Run commands sequentially, stopping on first failure.

## Installation

```bash
cargo install progcheck
```

Prebuilt binaries are also attached to each GitHub Release:
<https://github.com/ilya-epifanov/progcheck/releases>

## Usage

### Normal usage

```bash
progcheck -- "cargo fmt --check" "cargo clippy" "cargo test"
```

No output on success:
```bash
```

### Now break a test

```bash
progcheck -- "cargo fmt --check" "cargo clippy" "cargo test"
```

Output on failure:
```bash
FAILED: cargo test
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running unittests src/main.rs (target/debug/deps/progcheck-8f4ac9e26158bb38)

running 6 tests
test tests::keeps_start_and_end_when_truncated ... ok
test tests::keeps_complete_output_when_within_limit ... ok
test tests::parses_integer_exit_mode ... ok
test tests::rejects_invalid_exit_mode ... ok
test tests::parses_mirror_exit_mode ... ok
test tests::mirrors_signal_exit_as_128_plus_signal ... FAILED

failures:

---- tests::mirrors_signal_exit_as_128_plus_signal stdout ----

thread 'tests::mirrors_signal_exit_as_128_plus_signal' (757260) panicked at src/main.rs:470:9:
assertion `left == right` failed
  left: 143
 right: 123
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    tests::mirrors_signal_exit_as_128_plus_signal

test result: FAILED. 5 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--bin progcheck`
```

### Limit output to 100 chars just in case

```bash
progcheck -b 100 "cargo fmt --check" "cargo clippy" "cargo test"
```

Truncated output on failure:
```bash
FAILED: cargo test
Output truncated: omitted 899 characters; showing first 50 and last 50 characters.
    Finished `test` profile [unoptimized + debugin
... middle output omitted ...
ror: test failed, to rerun pass `--bin progcheck`
```

## Options

- `-e, --exit <CODE>` - Exit code on failure: integer (OS-dependent) or `mirror` (default: `1`)
- `-b, --buffer-size <CHARS>` - Number of output characters to retain for failed commands (default: `10000`)

## Behavior

- Commands run sequentially
- Output is captured while commands run
- Successful command output is discarded
- Only the first failed command's output is printed: `FAILED: <cmd>` then the captured stdout/stderr
- If output exceeds `--buffer-size`, `progcheck` shows:
  - an explicit truncation message at the top,
  - the beginning and end of output,
  - a `... middle output omitted ...` marker between them.

## Limitations

Child processes often buffer stdout when piped, so output ordering may not perfectly match the original write order.
