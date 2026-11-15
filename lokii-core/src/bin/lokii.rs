//! `lokii` 命令行：向正在运行的 Lokii App 查询实时索引。

use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::process::exit;
use std::time::Duration;

use lokii_core::query_server::{socket_path, Request, Response};
use lokii_core::search::SearchMode;

const USAGE: &str = "usage: lokii search [--json | --alfred] [-c|--content] [--cregex] [--sym] [--mode auto|substring|wildcard|regex|fuzzy] [--limit N] [--] <query...>";

// App 卡住（索引写锁排队等）时不让 Alfred / 脚本永久挂起
const IO_TIMEOUT: Duration = Duration::from_secs(5);

enum Format {
    Lines,
    Json,
    Alfred,
}

fn main() {
    // args_os + lossy：非 UTF-8 文件名（如 `lokii search *`）不应让进程 panic
    let mut args = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned());
    if args.next().as_deref() != Some("search") {
        fail(USAGE, 64);
    }
    let mut format = Format::Lines;
    let mut mode = None;
    let mut limit = None;
    let mut force_content = false;
    let mut force_regex = false;
    let mut force_symbol = false;
    let mut words = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => format = Format::Json,
            "--alfred" => format = Format::Alfred,
            "-c" | "--content" => force_content = true,
            "--cregex" => {
                force_content = true;
                force_regex = true;
            }
            "--sym" => {
                force_content = true;
                force_symbol = true;
            }
            "--mode" => {
                let value = args.next().unwrap_or_default();
                mode = Some(
                    serde_json::from_value::<SearchMode>(value.into())
                        .unwrap_or_else(|_| fail(USAGE, 64)),
                );
            }
            "--limit" => {
                limit = Some(
                    args.next()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or_else(|| fail(USAGE, 64)),
                );
            }
            "--" => {
                words.extend(args.by_ref());
                break;
            }
            // 拼错的选项不能静默当查询词；要搜以 - 开头的词用 `--` 分隔
            _ if arg.starts_with('-') => fail(USAGE, 64),
            _ => words.push(arg),
        }
    }
    if words.is_empty() {
        fail(USAGE, 64);
    }

    let request = Request {
        q: words.join(" "),
        mode,
        limit,
        case_sensitivity: None,
        force_content,
        force_symbol,
        force_regex,
        budget_ms: None,
    };
    let response = query(&request).unwrap_or_else(|e| {
        fail(
            &format!("lokii: cannot reach Lokii ({e}); make sure the Lokii app is running"),
            2,
        )
    });
    if let Some(error) = response.error {
        fail(&format!("lokii: {error}"), 1);
    }

    let mut out = BufWriter::new(std::io::stdout().lock());
    let written = if !response.content_hits.is_empty() {
        match format {
            Format::Lines => response.content_hits.iter().try_for_each(|h| {
                if h.line_matches.is_empty() {
                    writeln!(out, "{}", h.path)
                } else {
                    h.line_matches.iter().try_for_each(|m| {
                        writeln!(out, "{}:{}:{}: {}", h.path, m.line_number, m.column, m.line_text)
                    })
                }
            }),
            Format::Json => writeln!(
                out,
                "{}",
                serde_json::to_string(&response.content_hits).unwrap()
            ),
            Format::Alfred => {
                let items: Vec<_> = response
                    .content_hits
                    .iter()
                    .map(|h| {
                        let first_line = h.line_matches.first();
                        let title = first_line
                            .map(|m| m.line_text.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| {
                                std::path::Path::new(&h.path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| h.path.clone())
                            });
                        let subtitle = match first_line {
                            Some(m) => format!(
                                "{}:{} ({} matches)",
                                h.path, m.line_number, h.total_matches_in_file
                            ),
                            None => h.path.clone(),
                        };
                        serde_json::json!({
                            "uid": h.path,
                            "type": "file",
                            "title": title,
                            "subtitle": subtitle,
                            "arg": h.path,
                            "match": request.q,
                            "icon": { "type": "fileicon", "path": h.path },
                        })
                    })
                    .collect();
                writeln!(out, "{}", serde_json::json!({ "items": items }))
            }
        }
    } else {
        match format {
            Format::Lines => response
                .results
                .iter()
                .try_for_each(|e| writeln!(out, "{}", e.path)),
            Format::Json => writeln!(out, "{}", serde_json::to_string(&response.results).unwrap()),
            Format::Alfred => {
                let items: Vec<_> = response
                    .results
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "uid": e.path,
                            "type": "file",
                            "title": e.name,
                            "subtitle": e.path,
                            "arg": e.path,
                            // Alfred 勾选"过滤结果"时按此字段二次匹配；title 只有文件名，路径/fuzzy 查询会被滤空
                            "match": request.q,
                            "icon": { "type": "fileicon", "path": e.path },
                        })
                    })
                    .collect();
                writeln!(out, "{}", serde_json::json!({ "items": items }))
            }
        }
    };
    // EPIPE（如 `| head`）属正常截断；其余写错误（磁盘满等）不能带着截断输出以 0 退出
    if let Err(e) = written.and_then(|_| out.flush()) {
        if e.kind() != ErrorKind::BrokenPipe {
            fail(&format!("lokii: write failed: {e}"), 1);
        }
    }
}

fn query(request: &Request) -> std::io::Result<Response> {
    let mut stream = UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}

fn fail(message: &str, code: i32) -> ! {
    eprintln!("{message}");
    exit(code)
}
