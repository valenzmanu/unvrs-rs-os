//! The operator console's connection to the headless kernel: one event stream plus
//! one-shot commands. The console owns no kernel state.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read},
    os::unix::net::UnixStream,
    path::Path,
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant},
};
use uke::Universe;

pub struct Console {
    pub universe: Universe,
    token: String,
    stream: Option<UnixStream>,
}

impl Console {
    /// Starts the kernel if needed, attaches, returns the hello snapshot; events go to `tx`
    /// wrapped by `wrap`.
    pub fn attach<T: Send + 'static>(
        root: &Path,
        exe: &Path,
        tx: Sender<T>,
        wrap: fn(Value) -> T,
    ) -> Result<(Self, Value)> {
        let universe = Universe::at(root)?;
        universe.init()?;
        uke::ensure_running(&universe, exe, Duration::from_secs(10))?;
        let token_path = universe.kernel_dir().join("console.token");
        let until = Instant::now() + Duration::from_secs(5);
        let token = loop {
            if let Ok(t) = std::fs::read_to_string(&token_path)
                && !t.trim().is_empty()
            {
                break t.trim().to_owned();
            }
            ensure!(Instant::now() < until, "Kernel console token missing");
            thread::sleep(Duration::from_millis(20));
        };
        let mut stream = UnixStream::connect(universe.socket_path())
            .with_context(|| format!("Kernel socket {}", universe.socket_path().display()))?;
        uke::write_json(&mut stream, &json!({"op": "console", "token": token}))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut line = String::new();
        (&mut reader).take(64 * 1024 * 1024).read_line(&mut line)?;
        let hello: Value = serde_json::from_str(&line).context("Kernel console hello")?;
        if let Some(e) = hello.get("error").and_then(Value::as_str) {
            bail!("Kernel refused the console: {e}");
        }
        thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match (&mut reader).take(16 * 1024 * 1024).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    _ => {}
                }
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(wrap(v)).is_err()
                {
                    return;
                }
            }
            let _ = tx.send(wrap(json!({"ev": "closed"})));
        });
        Ok((
            Self {
                universe,
                token,
                stream: Some(stream),
            },
            hello,
        ))
    }

    /// One console command; errors come back as the kernel's refusal text.
    pub fn cmd(&self, cmd: &str, mut args: Value) -> Result<Value> {
        args["op"] = json!("console.cmd");
        args["cmd"] = json!(cmd);
        args["token"] = json!(self.token);
        uke::kernel_request(&self.universe, &args, Duration::from_secs(20))
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}
