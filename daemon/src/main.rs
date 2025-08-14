use tokio::net::{UnixListener, UnixStream};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use serde::Deserialize;
use serde::Serialize;
use std::os::unix::fs::PermissionsExt;

const SOCK_PATH: &str = "/run/loki-master.sock";

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Write { path: String, value: String },
    Run { program: String, args: Vec<String> },
}

#[derive(Serialize)]
struct Response {
    success: bool,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let _ = std::fs::remove_file(SOCK_PATH);
    let listener = UnixListener::bind(SOCK_PATH)?;
    // Make socket world-writable so unprivileged UI can connect
    let _ = std::fs::set_permissions(SOCK_PATH, std::fs::Permissions::from_mode(0o666));

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream).await {
                eprintln!("client error: {e}");
            }
        });
    }
}

async fn handle_client(stream: UnixStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let req: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            let mut stream = reader.into_inner();
            let resp = Response { success: false, error: Some(format!("parse error: {e}")) };
            let msg = serde_json::to_string(&resp)? + "\n";
            stream.write_all(msg.as_bytes()).await?;
            return Ok(());
        }
    };
    let resp = process_request(req).await;
    let mut stream = reader.into_inner();
    let msg = serde_json::to_string(&resp)? + "\n";
    stream.write_all(msg.as_bytes()).await?;
    Ok(())
}

async fn process_request(req: Request) -> Response {
    match req {
        Request::Write { path, value } => {
            match tokio::fs::write(&path, value).await {
                Ok(_) => Response { success: true, error: None },
                Err(e) => Response { success: false, error: Some(e.to_string()) },
            }
        }
        Request::Run { program, args } => {
            match tokio::process::Command::new(program)
                .args(args)
                .status()
                .await
            {
                Ok(status) => {
                    if status.success() {
                        Response { success: true, error: None }
                    } else {
                        Response {
                            success: false,
                            error: Some(format!("exit status: {status}")),
                        }
                    }
                }
                Err(e) => Response { success: false, error: Some(e.to_string()) },
            }
        }
    }
}
