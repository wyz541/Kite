use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

fn main() {
    let options = Options::parse();
    let mut samples = Vec::with_capacity(options.runs);
    for _ in 0..options.runs {
        let sample = run(&options).unwrap_or_else(|error| {
            eprintln!("kite-agent-perf: {error}");
            std::process::exit(1);
        });
        samples.push(sample);
    }
    let result = aggregate(&options, &samples);
    let encoded = serde_json::to_string_pretty(&result).expect("serialize result");
    if let Some(path) = options.output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create output directory");
        }
        std::fs::write(path, format!("{encoded}\n")).expect("write result");
    }
    println!("{encoded}");
}

struct Options {
    fixture: PathBuf,
    scenario: String,
    output: Option<PathBuf>,
    runs: usize,
}

impl Options {
    fn parse() -> Self {
        let mut fixture = sibling_fixture_path();
        let mut scenario = "long".to_string();
        let mut output = None;
        let mut runs = 3usize;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--fixture" => {
                    if let Some(value) = args.next() {
                        fixture = PathBuf::from(value);
                    }
                }
                "--scenario" => {
                    if let Some(value) = args.next() {
                        scenario = value;
                    }
                }
                "--output" => output = args.next().map(PathBuf::from),
                "--runs" => {
                    if let Some(value) = args.next() {
                        runs = value.parse::<usize>().unwrap_or(3).clamp(1, 20);
                    }
                }
                "--help" | "-h" => {
                    println!(
                        "Usage: kite-agent-perf [--fixture PATH] [--scenario long|slow|markdown] [--runs N] [--output FILE]"
                    );
                    std::process::exit(0);
                }
                _ => {}
            }
        }
        Self {
            fixture,
            scenario,
            output,
            runs,
        }
    }
}

fn aggregate(options: &Options, samples: &[Value]) -> Value {
    let mut system = System::new();
    system.refresh_cpu_all();
    let cpu_brand = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_string())
        .unwrap_or_default();
    let metric_keys = [
        "startup_ms",
        "output_bytes",
        "output_chunks",
        "output_elapsed_ms",
        "throughput_mib_per_second",
        "peak_agent_memory_bytes",
        "long_output_append_ms",
        "process_cleanup_ms",
    ];
    let mut metrics = serde_json::Map::new();
    for key in metric_keys {
        let mut values = samples
            .iter()
            .filter_map(|sample| {
                sample
                    .pointer(&format!("/metrics/{key}"))
                    .and_then(Value::as_f64)
            })
            .collect::<Vec<_>>();
        if values.is_empty() {
            continue;
        }
        values.sort_by(f64::total_cmp);
        let median = values[values.len() / 2];
        metrics.insert(
            key.to_string(),
            if key == "long_output_append_ms" || key == "throughput_mib_per_second" {
                json!(median)
            } else if key.ends_with("_bytes") || key.ends_with("_chunks") || key.ends_with("_ms") {
                json!(median.round() as u64)
            } else {
                json!(median)
            },
        );
    }
    json!({
        "schema_version": 2,
        "generated_unix_ms": unix_ms(),
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "scenario": options.scenario,
        "runs": options.runs,
        "environment": {
            "os_version": System::long_os_version(),
            "cpu_brand": cpu_brand,
            "logical_cores": system.cpus().len(),
        },
        "metrics": metrics,
        "samples": samples,
        "notes": [
            "Metrics are medians across independent fixture processes.",
            "The fixture measures transport and transcript preparation, not model latency.",
            "UI scroll frame timing is validated separately in the platform screenshot/performance pass."
        ]
    })
}

fn run(options: &Options) -> Result<Value, String> {
    let started = Instant::now();
    let mut child = Command::new(&options.fixture)
        .arg("--scenario")
        .arg(&options.scenario)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动 fixture {}：{error}", options.fixture.display()))?;
    let pid = child.id();
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "fixture stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "fixture stdout 不可用".to_string())?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    write_json(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": { "name": "kite-agent-perf", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
    )?;
    wait_for_id(&receiver, 1, Duration::from_secs(10))?;
    write_json(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": std::env::current_dir().map_err(|error| error.to_string())?, "mcpServers": [] }
        }),
    )?;
    wait_for_id(&receiver, 2, Duration::from_secs(10))?;
    let startup_ms = started.elapsed().as_millis() as u64;

    let output_started = Instant::now();
    write_json(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": "fixture-session",
                "prompt": [{ "type": "text", "text": "performance baseline" }]
            }
        }),
    )?;

    let mut system = System::new();
    let process_pid = Pid::from_u32(pid);
    let mut peak_memory = 0u64;
    let mut output_bytes = 0usize;
    let mut chunks = 0usize;
    let mut transcript = VecDeque::<String>::new();
    let mut append_duration = Duration::ZERO;
    let mut last_memory_sample = Instant::now() - Duration::from_secs(1);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if last_memory_sample.elapsed() >= Duration::from_millis(200) {
            refresh_process_memory(&mut system, process_pid, &mut peak_memory);
            last_memory_sample = Instant::now();
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("fixture prompt 超时".to_string());
        }
        let line = match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("fixture stdout 已关闭".to_string());
            }
        };
        let value = serde_json::from_str::<Value>(&line)
            .map_err(|error| format!("fixture 输出不是 JSON：{error}"))?;
        if value.get("id").and_then(Value::as_u64) == Some(3) {
            if let Some(error) = value.get("error") {
                return Err(format!("fixture prompt 失败：{error}"));
            }
            break;
        }
        if let Some(text) = value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
        {
            let append_started = Instant::now();
            transcript.push_back(text.to_string());
            while transcript.len() > 2_000 {
                transcript.pop_front();
            }
            append_duration += append_started.elapsed();
            output_bytes += text.len();
            chunks += 1;
        }
    }
    refresh_process_memory(&mut system, process_pid, &mut peak_memory);
    let output_elapsed = output_started.elapsed();
    let throughput =
        output_bytes as f64 / 1024.0 / 1024.0 / output_elapsed.as_secs_f64().max(0.001);
    let cleanup_started = Instant::now();
    let _ = child.kill();
    let _ = child.wait();
    let cleanup_ms = cleanup_started.elapsed().as_millis() as u64;

    Ok(json!({
        "schema_version": 1,
        "generated_unix_ms": unix_ms(),
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "scenario": options.scenario,
        "metrics": {
            "startup_ms": startup_ms,
            "output_bytes": output_bytes,
            "output_chunks": chunks,
            "output_elapsed_ms": output_elapsed.as_millis() as u64,
            "throughput_mib_per_second": throughput,
            "peak_agent_memory_bytes": peak_memory,
            "long_output_append_ms": append_duration.as_secs_f64() * 1000.0,
            "process_cleanup_ms": cleanup_ms
        },
        "notes": [
            "The fixture measures transport and transcript preparation, not model latency.",
            "UI scroll frame timing is validated separately in the platform screenshot/performance pass."
        ]
    }))
}

fn wait_for_id(
    receiver: &mpsc::Receiver<String>,
    id: u64,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("等待 ACP 响应 {id} 超时"));
        }
        let Ok(line) = receiver.recv_timeout(remaining) else {
            return Err(format!("fixture 在响应 {id} 前关闭"));
        };
        let value = serde_json::from_str::<Value>(&line)
            .map_err(|error| format!("fixture 输出不是 JSON：{error}"))?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = value.get("error") {
                return Err(format!("fixture 响应 {id} 失败：{error}"));
            }
            return Ok(());
        }
    }
}

fn write_json(stdin: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdin, value).map_err(|error| error.to_string())?;
    stdin
        .write_all(b"\n")
        .and_then(|_| stdin.flush())
        .map_err(|error| error.to_string())
}

fn refresh_process_memory(system: &mut System, pid: Pid, peak: &mut u64) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    if let Some(process) = system.process(pid) {
        *peak = (*peak).max(process.memory());
    }
}

fn sibling_fixture_path() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kite-agent-perf"));
    executable.with_file_name(if cfg!(windows) {
        "kite-agent-fixture.exe"
    } else {
        "kite-agent-fixture"
    })
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
