//! `ccs serve --stdio`: the remote end of the protocol in `wire`.

use super::server;
use super::wire::{WireHello, WireRequest, WireResponse};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Runs one request: method, params, cancel flag.
pub(crate) type Handler = Arc<
    dyn Fn(&str, serde_json::Value, &Arc<AtomicBool>) -> Result<serde_json::Value, String>
        + Send
        + Sync,
>;

/// Serve requests from `input` until EOF, replies on `output`. Each request
/// runs on its own thread so a `cancel` line is read while a search runs.
pub fn run_stdio<R: BufRead, W: Write + Send + 'static>(
    input: R,
    output: W,
    search_paths: Vec<String>,
    home: String,
) -> i32 {
    run_with(input, output, default_handler(search_paths, home))
}

fn default_handler(search_paths: Vec<String>, home: String) -> Handler {
    Arc::new(move |method, params, cancel| dispatch(method, params, cancel, &search_paths, &home))
}

fn dispatch(
    method: &str,
    params: serde_json::Value,
    cancel: &Arc<AtomicBool>,
    search_paths: &[String],
    home: &str,
) -> Result<serde_json::Value, String> {
    fn parse<T: serde::de::DeserializeOwned>(params: serde_json::Value) -> Result<T, String> {
        serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))
    }
    fn reply<T: serde::Serialize>(value: T) -> Result<serde_json::Value, String> {
        serde_json::to_value(value).map_err(|e| e.to_string())
    }
    match method {
        "search" => reply(server::search(&parse(params)?, search_paths, home, cancel)?),
        "recent" => reply(server::recent(&parse(params)?, search_paths, home)),
        "show" => reply(server::show(&parse(params)?)?),
        "tree" => reply(server::tree(&parse(params)?)?),
        other => Err(format!("unknown method: {other}")),
    }
}

type Registry = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

pub(crate) fn run_with<R: BufRead, W: Write + Send + 'static>(
    input: R,
    output: W,
    handler: Handler,
) -> i32 {
    let output = Arc::new(Mutex::new(output));
    if write_line(&output, &WireHello::current()).is_err() {
        return 1;
    }
    let registry: Registry = Arc::default();
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    for line in input.lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value["method"] == "cancel" {
            cancel_request(&registry, value["params"]["id"].as_u64());
            continue;
        }
        if let Ok(request) = serde_json::from_value::<WireRequest>(value) {
            workers.push(spawn_request(request, &handler, &registry, &output));
        }
    }
    for flag in lock(&registry).values() {
        flag.store(true, Ordering::Relaxed);
    }
    for worker in workers {
        let _ = worker.join();
    }
    0
}

fn cancel_request(registry: &Registry, id: Option<u64>) {
    if let Some(flag) = id.and_then(|id| lock(registry).get(&id).cloned()) {
        flag.store(true, Ordering::Relaxed);
    }
}

fn spawn_request<W: Write + Send + 'static>(
    request: WireRequest,
    handler: &Handler,
    registry: &Registry,
    output: &Arc<Mutex<W>>,
) -> JoinHandle<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    lock(registry).insert(request.id, cancel.clone());
    let (handler, registry, output) = (handler.clone(), registry.clone(), output.clone());
    std::thread::spawn(move || {
        let id = request.id;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            handler(&request.method, request.params, &cancel)
        }))
        .unwrap_or_else(|panic| Err(format!("panic: {}", panic_text(&panic))));
        lock(&registry).remove(&id);
        // A cancelled request gets no reply; the client already dropped it.
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let (result, error) = match outcome {
            Ok(value) => (Some(value), None),
            Err(e) => (None, Some(e)),
        };
        let _ = write_line(&output, &WireResponse { id, result, error });
    })
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".to_string())
}

/// One whole line per message; a panicked writer elsewhere does not stop us.
fn write_line<W: Write, T: serde::Serialize>(
    output: &Arc<Mutex<W>>,
    value: &T,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut out = lock(output);
    out.write_all(line.as_bytes())?;
    out.flush()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SharedBuf {
        fn lines(&self) -> Vec<serde_json::Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    /// Input that holds EOF back until `out` has `lines` lines, the way a
    /// client keeps stdin open while it waits for replies.
    struct GatedInput {
        data: std::io::Cursor<Vec<u8>>,
        out: SharedBuf,
        lines: usize,
        /// Extra time stdin stays open after the lines appeared.
        linger: std::time::Duration,
    }

    impl std::io::Read for GatedInput {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while self.out.lines().len() < self.lines && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            std::thread::sleep(self.linger);
            Ok(0)
        }
    }

    fn gated(input: &str, out: &SharedBuf, lines: usize) -> std::io::BufReader<GatedInput> {
        gated_for(input, out, lines, std::time::Duration::ZERO)
    }

    fn gated_for(
        input: &str,
        out: &SharedBuf,
        lines: usize,
        linger: std::time::Duration,
    ) -> std::io::BufReader<GatedInput> {
        std::io::BufReader::new(GatedInput {
            data: std::io::Cursor::new(input.as_bytes().to_vec()),
            out: out.clone(),
            lines,
            linger,
        })
    }

    fn ids(lines: &[serde_json::Value]) -> Vec<u64> {
        lines.iter().filter_map(|l| l["id"].as_u64()).collect()
    }

    #[test]
    fn serve_when_cancel_sent_before_reply_then_no_reply_for_that_id() {
        // The search gives up waiting after 100 ms and would reply; only the
        // cancel line (not the EOF sweep, 300 ms later) can stop that reply.
        let handler: Handler = Arc::new(|method, _params, cancel| {
            if method == "search" {
                let until = std::time::Instant::now() + std::time::Duration::from_millis(100);
                while !cancel.load(Ordering::Relaxed) && std::time::Instant::now() < until {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            Ok(serde_json::json!({}))
        });
        let input = concat!(
            r#"{"id":1,"method":"search","params":{}}"#,
            "\n",
            r#"{"method":"cancel","params":{"id":1}}"#,
            "\n",
            r#"{"id":2,"method":"show","params":{}}"#,
            "\n"
        );
        let out = SharedBuf::default();
        let input = gated_for(input, &out, 2, std::time::Duration::from_millis(300));
        assert_eq!(run_with(input, out.clone(), handler), 0);
        let lines = out.lines();
        assert_eq!(lines[0]["type"], "hello");
        assert_eq!(ids(&lines), vec![2]);
    }

    #[test]
    fn serve_when_panicking_handler_then_error_response_and_server_alive() {
        let handler: Handler = Arc::new(|method, _params, _cancel| {
            if method == "search" {
                panic!("kaboom");
            }
            Ok(serde_json::json!({"ok": true}))
        });
        let input = concat!(
            r#"{"id":1,"method":"search","params":{}}"#,
            "\n",
            r#"{"id":2,"method":"show","params":{}}"#,
            "\n"
        );
        let out = SharedBuf::default();
        run_with(gated(input, &out, 3), out.clone(), handler);
        let lines = out.lines();
        let first = lines.iter().find(|l| l["id"] == 1).unwrap();
        assert!(first["error"].as_str().unwrap().contains("panic: kaboom"));
        let second = lines.iter().find(|l| l["id"] == 2).unwrap();
        assert_eq!(second["result"]["ok"], true);
    }

    #[test]
    fn serve_when_line_is_not_json_then_skipped() {
        let handler: Handler = Arc::new(|_, _, _| Ok(serde_json::json!(1)));
        let input = "garbage\n{\"id\":3,\"method\":\"show\",\"params\":{}}\n";
        let out = SharedBuf::default();
        run_with(gated(input, &out, 2), out.clone(), handler);
        assert_eq!(ids(&out.lines()), vec![3]);
    }
}
